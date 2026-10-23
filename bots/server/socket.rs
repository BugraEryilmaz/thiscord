use super::{events, store};
use crate::{
    auth::{self, Failure},
    chat::access,
    db::DbPool,
};
use axum::{
    Extension,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{ApiError, ErrorCode, RequestId, bots::*};
use tokio::sync::Semaphore;

pub(super) async fn upgrade(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(origins): Extension<Arc<Vec<HeaderValue>>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    // Native clients may omit Origin. Browsers must use an explicitly allowed one.
    if headers.get("origin").is_some_and(|o| !origins.contains(o)) {
        return Failure::Forbidden.response(id);
    }
    let Some(pool) = pool else {
        return Failure::Unavailable.response(id);
    };
    let Ok(ws) = ws else {
        return Failure::Invalid("Invalid bot socket upgrade").response(id);
    };
    static SOCKETS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let Ok(permit) = SOCKETS
        .get_or_init(|| Arc::new(Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
    else {
        return Failure::Limited.response(id);
    };
    ws.max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            serve(socket, pool, id).await
        })
        .into_response()
}
fn frame(message: Message) -> Option<ClientEvent> {
    let Message::Text(text) = message else {
        return None;
    };
    let frame: ClientFrame = serde_json::from_str(&text).ok()?;
    (frame.version == SOCKET_VERSION).then_some(frame.event)
}
async fn send(socket: &mut WebSocket, event: ServerEvent) -> Result<(), ()> {
    let text = serde_json::to_string(&ServerFrame {
        version: SOCKET_VERSION,
        event,
    })
    .map_err(|_| ())?;
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
async fn invalid(socket: &mut WebSocket, id: RequestId, code: ErrorCode, message: &str) {
    let _ = send(
        socket,
        ServerEvent::Error {
            error: ApiError {
                code,
                message: message.into(),
                request_id: id,
                fields: vec![],
            },
        },
    )
    .await;
}
async fn serve(mut socket: WebSocket, pool: DbPool, id: RequestId) {
    let Ok(Some(Ok(first))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Some(ClientEvent::Authenticate {
        token,
        mut channels,
    }) = frame(first)
    else {
        invalid(
            &mut socket,
            id,
            ErrorCode::BadRequest,
            "Authenticate with bot socket version 1 first",
        )
        .await;
        return;
    };
    if channels.is_empty() || channels.len() > MAX_CHANNELS {
        invalid(
            &mut socket,
            id,
            ErrorCode::BadRequest,
            "Select between 1 and 32 channels",
        )
        .await;
        return;
    }
    channels.sort();
    channels.dedup();
    let p = pool.clone();
    let t = token.clone();
    let result = crate::performance::blocking(move || {
        let mut c = auth::store::connection(&p)?;
        let session = super::authenticate(&mut c, &t)?;
        let bot = store::info(&mut c, session.account_id)?;
        drop(c);
        auth::store::rate_limit(
            &p,
            &format!("bot-socket:{}", session.account_id),
            None,
            true,
        )?;
        Ok::<_, Failure>((session, bot))
    })
    .await;
    let Ok(Ok((session, bot))) = result else {
        invalid(
            &mut socket,
            id,
            ErrorCode::Unauthorized,
            "Invalid bot credential",
        )
        .await;
        return;
    };
    let account = access::account(bot.account_id);
    let credential = access::session(session.id);
    let guild = access::guild(bot.guild_id);
    let member = access::member(bot.guild_id, bot.account_id);
    let mut account_changed = account.subscribe();
    let mut session_changed = credential.subscribe();
    let mut guild_changed = guild.subscribe();
    let mut member_changed = member.subscribe();
    let snapshot = || {
        access::snapshot(&account, Some(&guild))?
            .including(&credential)?
            .including(&member)
    };
    let Some(initial) = snapshot() else {
        return;
    };
    let p = pool.clone();
    let t = token.clone();
    let selected = channels.clone();
    let gid = bot.guild_id;
    if !matches!(
        crate::performance::blocking(move || store::checked(&p, &t, gid, &selected)).await,
        Ok(Ok(()))
    ) {
        invalid(
            &mut socket,
            id,
            ErrorCode::Forbidden,
            "Channel access denied",
        )
        .await;
        return;
    }
    let mut messages = events().subscribe();
    {
        let Some(_delivery) = initial.deliver() else {
            return;
        };
        if send(&mut socket, ServerEvent::Ready { bot: bot.clone() })
            .await
            .is_err()
        {
            return;
        }
    }
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0;
    loop {
        tokio::select! { biased;
            _ = account_changed.changed() => break,
            _ = session_changed.changed() => break,
            _ = guild_changed.changed() => break,
            _ = member_changed.changed() => break,
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { return; };
                if matches!(incoming,Message::Close(_)) { return; }
                if window.elapsed() > Duration::from_secs(10) { window=Instant::now(); count=0; }
                count+=1; if count > 30 { invalid(&mut socket,id,ErrorCode::RateLimited,"Socket event limit exceeded").await; return; }
                last_seen=Instant::now();
                match incoming {
                    Message::Ping(_) | Message::Pong(_) => {},
                    message => if matches!(frame(message),Some(ClientEvent::Ping{})) {
                        if send(&mut socket,ServerEvent::Pong{}).await.is_err() { return; }
                    } else { invalid(&mut socket,id,ErrorCode::BadRequest,"Invalid bot event").await; return; }
                }
            }
            message = messages.recv() => {
                let Ok(message) = message else { invalid(&mut socket,id,ErrorCode::ServiceUnavailable,"Bot event queue overflow; reconnect").await; return; };
                if message.guild_id != bot.guild_id || !channels.contains(&message.channel_id) || !message.mentions.contains(&bot.account_id) { continue; }
                let Some(permit) = snapshot() else { break; };
                let p=pool.clone(); let t=token.clone(); let ch=message.channel_id;
                if !matches!(crate::performance::blocking(move || store::checked(&p,&t,gid,&[ch])).await,Ok(Ok(()))) { break; }
                let Some(_delivery) = permit.deliver() else { break; };
                if send(&mut socket,ServerEvent::Mention { message }).await.is_err() { return; }
            }
            _ = tick.tick() => {
                if last_seen.elapsed()>Duration::from_secs(35) { return; }
                let p=pool.clone(); let t=token.clone(); let selected=channels.clone();
                if !matches!(crate::performance::blocking(move || store::checked(&p,&t,gid,&selected)).await,Ok(Ok(()))) { break; }
            }
        }
    }
    let _ = send(&mut socket, ServerEvent::Revoked {}).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}
