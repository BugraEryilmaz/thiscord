mod notifications;
mod session;
mod store;
use crate::{
    auth::{self, Failure},
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, State, WebSocketUpgrade,
        rejection::JsonRejection,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use notifications::notifications;
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{ApiError, ErrorCode, RequestId, chat::*, permissions::Permission};
use tokio::sync::{RwLock, Semaphore, watch};
use uuid::Uuid;

pub(crate) fn gate() -> &'static RwLock<()> {
    static GATE: OnceLock<RwLock<()>> = OnceLock::new();
    GATE.get_or_init(|| RwLock::new(()))
}
pub(crate) fn changes() -> &'static watch::Sender<u64> {
    static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| watch::channel(0).0)
}
pub(crate) fn invalidate() {
    changes().send_modify(|v| *v = v.wrapping_add(1));
}
pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    Router::new()
        .route(CHAT_PATH, post(handle))
        .route(SOCKET_PATH, get(upgrade))
        .layer(Extension(Arc::new(origins)))
        .layer(DefaultBodyLimit::max(32 * 1024))
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Response {
    let _guard = gate().write().await;
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid chat request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(8)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (response, change) = store::dispatch(&pool, &token, command)?;
            // A started blocking worker survives cancellation of the HTTP future.
            // Publish after commit here so its durable write still reaches sockets.
            if let Some(change) = change {
                notifications().publish(change);
            }
            Ok::<_, Failure>(response)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let mut response = match result {
        Ok(body) => Json(body).into_response(),
        Err(e) => e.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
async fn upgrade(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(origins): Extension<Arc<Vec<HeaderValue>>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !headers.get("origin").is_some_and(|o| origins.contains(o)) {
        return Failure::Forbidden.response(id);
    }
    let Some(pool) = pool else {
        return Failure::Unavailable.response(id);
    };
    let Ok(ws) = ws else {
        return Failure::Invalid("Invalid WebSocket upgrade").response(id);
    };
    static SOCKETS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let Ok(permit) = SOCKETS
        .get_or_init(|| Arc::new(Semaphore::new(128)))
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
}
async fn send(socket: &mut WebSocket, event: ServerEvent) -> Result<(), ()> {
    let data = serde_json::to_string(&ServerFrame {
        version: SOCKET_VERSION,
        event,
    })
    .map_err(|_| ())?;
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(data.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
fn error(id: RequestId, code: ErrorCode, message: &str) -> ServerEvent {
    ServerEvent::Error {
        error: ApiError {
            code,
            message: message.into(),
            request_id: id,
            fields: vec![],
        },
    }
}
fn frame(message: Message) -> Option<ClientEvent> {
    let Message::Text(text) = message else {
        return None;
    };
    let frame: ClientFrame = serde_json::from_str(&text).ok()?;
    (frame.version == SOCKET_VERSION).then_some(frame.event)
}
async fn serve(mut socket: WebSocket, pool: DbPool, id: RequestId) {
    let mut changed = changes().subscribe();
    let first = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let Ok(Some(Ok(message))) = first else {
        return;
    };
    let event = frame(message);
    if let Some(ClientEvent::Connect { token }) = event {
        session::serve(socket, pool, id, token, changed).await;
        return;
    }
    let Some(ClientEvent::Authenticate {
        token,
        guild_id,
        channel_id,
    }) = event
    else {
        let _ = send(
            &mut socket,
            error(
                id,
                ErrorCode::BadRequest,
                "Authenticate with socket version 1 first",
            ),
        )
        .await;
        return;
    };
    let connection_id = Uuid::new_v4();
    let mut cursor;
    {
        let _guard = gate().write().await;
        if changed.has_changed().unwrap_or(true) {
            return;
        }
        let p = pool.clone();
        let t = token.clone();
        let ready = tokio::task::spawn_blocking(move || {
            let session = auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)?;
            auth::store::rate_limit(&p, &format!("socket:{}", session.account_id), None, true)?;
            store::checked(
                &p,
                &t,
                guild_id,
                Some(channel_id),
                Some(Permission::ReadHistory),
                |c, _, _| store::history(c, guild_id, channel_id, None, Default::default()),
            )
        })
        .await;
        let Ok(Ok(history)) = ready else {
            let _ = send(
                &mut socket,
                error(
                    id,
                    ErrorCode::Forbidden,
                    "Session or channel access is unavailable",
                ),
            )
            .await;
            return;
        };
        cursor = history.event_cursor;
        if send(&mut socket, ServerEvent::Ready { history })
            .await
            .is_err()
        {
            return;
        }
    }
    let mut tick = tokio::time::interval(Duration::from_millis(750));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0u32;
    let mut typing = None;
    let mut last_members = Vec::new();
    loop {
        tokio::select! {biased;
            _=changed.changed()=>{let _=send(&mut socket,ServerEvent::Revoked{}).await;break;}
            message=socket.recv()=>{
                let Some(Ok(message))=message else{break;};
                if matches!(message,Message::Close(_)){break;}
                if window.elapsed()>Duration::from_secs(10){window=Instant::now();count=0;}
                count+=1;if count>30{let _=send(&mut socket,error(id,ErrorCode::RateLimited,"Socket event limit exceeded")).await;break;}
                last_seen=Instant::now();
                match frame(message){
                    Some(ClientEvent::Ping{})=>{if send(&mut socket,ServerEvent::Pong{}).await.is_err(){break;}}
                    Some(ClientEvent::Typing{active})=>typing=Some(active),
                    _=>{let _=send(&mut socket,error(id,ErrorCode::BadRequest,"Invalid socket event")).await;break;}
                }
            }
            _=tick.tick()=>{
                if last_seen.elapsed()>Duration::from_secs(35){break;}
                let _guard=gate().read().await;
                if changed.has_changed().unwrap_or(true){let _=send(&mut socket,ServerEvent::Revoked{}).await;break;}
                let p=pool.clone();let t=token.clone();let active=typing.take();
                let result=tokio::task::spawn_blocking(move||store::poll(&p,&t,guild_id,channel_id,cursor,connection_id,active)).await;
                let Ok(Ok(poll))=result else{let _=send(&mut socket,ServerEvent::Revoked{}).await;break;};
                let delivery=async {
                    for (seq,message) in poll.events{send(&mut socket,ServerEvent::Message{message,cursor:seq}).await?;cursor=seq;}
                    if poll.members!=last_members{last_members=poll.members.clone();send(&mut socket,ServerEvent::Presence{members:poll.members}).await?;}
                    Ok::<(),()>(())
                };
                if !matches!(tokio::time::timeout(Duration::from_secs(2),delivery).await,Ok(Ok(()))){break;}
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    let _ = tokio::task::spawn_blocking(move || store::cleanup(&pool, connection_id)).await;
}
