//! Transfer authenticated signaling sockets to the existing media owner.
use super::*;

#[derive(Clone)]
pub(super) struct Handle {
    pub secret: String,
    guild: GuildId,
    sequence: u64,
    sender: mpsc::Sender<Incoming>,
}
pub(super) struct Incoming {
    pub socket: logging::Socket,
    pub sequence: u64,
}
pub(super) fn channel(guild: GuildId) -> (Handle, mpsc::Receiver<Incoming>) {
    let (sender, receiver) = mpsc::channel(1);
    (
        Handle {
            secret: uuid::Uuid::new_v4().to_string(),
            guild,
            sequence: 0,
            sender,
        },
        receiver,
    )
}

pub(super) async fn current(room: &Room, slot: usize, source: uuid::Uuid, sequence: u64) -> bool {
    room.members.read().await.get(&slot).is_some_and(|member| {
        member.source_id == source
            && member.active.load(Ordering::Acquire)
            && member
                .resumption
                .as_ref()
                .is_some_and(|handle| handle.sequence == sequence)
    })
}

pub(super) async fn attach(
    mut socket: logging::Socket,
    pool: &DbPool,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    secret: String,
    sequence: u64,
) {
    let id = socket.id;
    socket.stage = "resume_authorize";
    let result = async {
        let (_, access) = access::identify(pool, &token, Some(guild)).await?;
        let mut changed = access.subscribe();
        let ((info, _), epoch) = access
            .authorize(&mut changed, || {
                let pool = pool.clone();
                let token = token.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        store::authorize(&pool, &token, guild, channel)
                    })
                    .await
                    .unwrap_or(Err(Failure::Unavailable))
                }
            })
            .await?;
        let limit_pool = pool.clone();
        let limit_key = format!("voice-resume:{}", info.account_id);
        tokio::task::spawn_blocking(move || {
            crate::auth::store::rate_limit(&limit_pool, &limit_key, None, true)
        })
        .await
        .unwrap_or(Err(Failure::Unavailable))?;
        let room = room(channel).await;
        let mut members = room.members.write().await;
        let _admission = access.packet(epoch, epoch).ok_or(Failure::Unavailable)?;
        let Some(member) = members
            .values_mut()
            .find(|m| m.info.account_id == info.account_id)
        else {
            return Ok(Err(ServerEvent::ResumeUnavailable {}));
        };
        let old_epoch = member.generation.load(Ordering::Acquire);
        let _original_access = member
            .access
            .packet(old_epoch, old_epoch)
            .ok_or(Failure::Unavailable)?;
        let Some(handle) = member.resumption.as_mut() else {
            return Err(Failure::Forbidden);
        };
        if handle.guild != guild
            || handle.secret != secret
            || !member.active.load(Ordering::Acquire)
        {
            return Ok(Err(ServerEvent::Replaced {}));
        }
        if sequence <= handle.sequence {
            return Err(Failure::Conflict);
        }
        // Reserve before advancing the fence. A full queue cannot invalidate the
        // recovery already waiting for the media owner to process it.
        let permit = handle
            .sender
            .clone()
            .try_reserve_owned()
            .map_err(|_| Failure::Unavailable)?;
        handle.sequence = sequence;
        Ok(Ok(permit))
    }
    .await;
    match result {
        Ok(Ok(permit)) => {
            permit.send(Incoming { socket, sequence });
        }
        Ok(Err(event)) => {
            let _ = send(&mut socket, event).await;
        }
        Err(error) => fail(&mut socket, id, error).await,
    }
}
