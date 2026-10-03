use super::*;
use thiscord_shared::{ChannelId, GuildId, permissions::Permissions};

#[derive(Clone, Copy)]
struct Subscription {
    id: u64,
    guild: GuildId,
    channel: Option<ChannelId>,
}
struct Snapshot {
    history: Option<History>,
    permissions: Permissions,
    unread: Vec<Unread>,
}
fn snapshot(pool: &DbPool, token: &str, sub: Subscription) -> Result<Snapshot, Failure> {
    store::checked(
        pool,
        token,
        sub.guild,
        sub.channel,
        Some(Permission::ReadHistory),
        |c, actor, state| {
            Ok(Snapshot {
                history: sub
                    .channel
                    .map(|channel| store::history(c, sub.guild, channel, None, Default::default()))
                    .transpose()?,
                permissions: crate::permissions::evaluator::effective(state, actor, sub.channel),
                unread: store::unread(c, sub.guild, actor, state)?,
            })
        },
    )
}
async fn update(socket: &mut WebSocket, sub: Subscription, event: ServerEvent) -> Result<(), ()> {
    send(
        socket,
        ServerEvent::Update {
            subscription: sub.id,
            event: Box::new(event),
        },
    )
    .await
}

pub(super) async fn serve(
    mut socket: WebSocket,
    pool: DbPool,
    id: RequestId,
    token: String,
    mut revoked: watch::Receiver<u64>,
) {
    let mut updates = updates().subscribe();
    let connection = Uuid::new_v4();
    let p = pool.clone();
    let t = token.clone();
    let authenticated = tokio::task::spawn_blocking(move || {
        let session = auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)?;
        auth::store::rate_limit(&p, &format!("socket:{}", session.account_id), None, true)
    })
    .await;
    if !matches!(authenticated, Ok(Ok(_))) {
        let _ = send(
            &mut socket,
            error(id, ErrorCode::Unauthorized, "Sign in again"),
        )
        .await;
        return;
    }
    if send(&mut socket, ServerEvent::Authenticated {})
        .await
        .is_err()
    {
        return;
    }
    let mut subscription: Option<Subscription> = None;
    let mut cursor = 0;
    let mut last_members = Vec::new();
    let mut last_unread = Vec::new();
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0;
    let mut dirty = false;
    let mut typing = false;
    let mut typing_expiry = None::<Instant>;
    let mut maintenance = tokio::time::interval(Duration::from_secs(10));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            _ = revoked.changed() => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { break; };
                if matches!(incoming, Message::Close(_)) { break; }
                if window.elapsed() >= Duration::from_secs(10) { window = Instant::now(); count = 0; }
                count += 1;
                if count > 30 { let _ = send(&mut socket, error(id, ErrorCode::RateLimited, "Socket event limit exceeded")).await; break; }
                last_seen = Instant::now();
                match frame(incoming) {
                    Some(ClientEvent::Subscribe { subscription: serial, guild_id, channel_id }) => {
                        let _guard = gate().read().await;
                        if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                        let p = pool.clone();
                        let _ = tokio::task::spawn_blocking(move || store::cleanup(&p, connection)).await;
                        notify();
                        subscription = None; typing = false; typing_expiry = None;
                        last_members.clear(); last_unread.clear();
                        if channel_id.is_some() && guild_id.is_none() {
                            let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "A channel requires a guild")).await; break;
                        }
                        if let Some(guild) = guild_id {
                            let sub = Subscription { id: serial, guild, channel: channel_id };
                            let p = pool.clone(); let t = token.clone();
                            let result = tokio::task::spawn_blocking(move || snapshot(&p, &t, sub)).await;
                            let Ok(Ok(ready)) = result else {
                                let _ = update(&mut socket, sub, error(id, ErrorCode::Forbidden, "Session or channel access is unavailable")).await;
                                continue;
                            };
                            cursor = ready.history.as_ref().map_or(0, |h| h.event_cursor);
                            last_unread = ready.unread.clone();
                            if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: ready.history, permissions: ready.permissions }).await.is_err() { break; }
                            if update(&mut socket, sub, ServerEvent::Unread { channels: ready.unread }).await.is_err() { break; }
                            subscription = Some(sub); dirty = true;
                        } else if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: None, permissions: Permissions::new() }).await.is_err() { break; }
                    }
                    Some(ClientEvent::Ping {}) => { if send(&mut socket, ServerEvent::Pong {}).await.is_err() { break; } }
                    Some(ClientEvent::Typing { active }) if subscription.is_some_and(|s| s.channel.is_some()) => {
                        typing = active; dirty = true;
                        typing_expiry = Some(Instant::now() + Duration::from_secs(4));
                        notify();
                    }
                    _ => { let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "Invalid socket event")).await; break; }
                }
            }
            _ = updates.changed() => { dirty = true; }
            _ = async {}, if dirty => {
                dirty = false;
                let Some(sub) = subscription else { continue; };
                let _guard = gate().read().await;
                if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                let p = pool.clone(); let t = token.clone(); let active = typing; typing = false;
                let result = tokio::task::spawn_blocking(move || {
                    let unread = store::checked(&p, &t, sub.guild, None, None, |c, actor, state| store::unread(c, sub.guild, actor, state))?;
                    let poll = sub.channel.map(|ch| store::poll(&p, &t, sub.guild, ch, cursor, connection, active)).transpose()?;
                    Ok::<_, Failure>((unread, poll))
                }).await;
                let Ok(Ok((unread, poll))) = result else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                let delivery = async {
                    if unread != last_unread {
                        last_unread = unread.clone();
                        update(&mut socket, sub, ServerEvent::Unread { channels: unread }).await?;
                    }
                    if let Some(poll) = poll {
                        dirty = poll.events.len() == 100;
                        for (sequence, message) in poll.events {
                            update(&mut socket, sub, ServerEvent::Message { message, cursor: sequence }).await?;
                            cursor = sequence;
                        }
                        if poll.members != last_members {
                            // Membership and typing updates wake other subscribers once.
                            last_members = poll.members.clone();
                            update(&mut socket, sub, ServerEvent::Presence { members: poll.members }).await?;
                            notify();
                        }
                    }
                    Ok::<(), ()>(())
                };
                if !matches!(tokio::time::timeout(Duration::from_secs(2), delivery).await, Ok(Ok(()))) { break; }
            }
            _ = async { if let Some(at) = typing_expiry { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                typing_expiry = None; notify();
            }
            _ = maintenance.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) { break; }
                // Expiry/authentication and presence lease maintenance only;
                // committed changes independently wake delivery immediately.
                let p = pool.clone(); let t = token.clone();
                if !matches!(tokio::task::spawn_blocking(move || auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)).await, Ok(Ok(_))) { break; }
                if let Some(sub) = subscription {
                    let _guard = gate().read().await;
                    let p = pool.clone(); let t = token.clone();
                    match tokio::task::spawn_blocking(move || store::maintain(&p, &t, sub.guild, sub.channel, connection)).await {
                        Ok(Ok(true)) => notify(),
                        Ok(Ok(false)) => {},
                        _ => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                    }
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    let _ = tokio::task::spawn_blocking(move || store::cleanup(&pool, connection)).await;
    notify();
}
