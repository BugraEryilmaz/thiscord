mod attachments;
pub(crate) mod store;
mod typing;
use crate::{
    auth::{self, Failure},
    db::DbPool,
};
pub use attachments::start_cleanup as start_attachment_cleanup;
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, State, WebSocketUpgrade,
        rejection::JsonRejection,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response as HttpResponse},
    routing::{get, post},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{AccountId, RequestId, friends::*};
use tokio::sync::{Semaphore, watch};

/// Database snapshots are rebuilt on commit notifications, call expiry and at
/// least this often (session expiry, writes outside this module).
const REFRESH: Duration = Duration::from_secs(10);

pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    Router::new()
        .route(FRIENDS_PATH, post(handle))
        .route(FRIENDS_SOCKET_PATH, get(upgrade))
        .layer(Extension(Arc::new(origins)))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .merge(attachments::router())
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<Request>, JsonRejection>,
) -> HttpResponse {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid friends request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(8)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?;
        crate::performance::blocking(move || {
            let _permit = permit;
            store::dispatch(&pool, &token, command)
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
fn presence() -> &'static Mutex<HashMap<AccountId, usize>> {
    static PRESENCE: OnceLock<Mutex<HashMap<AccountId, usize>>> = OnceLock::new();
    PRESENCE.get_or_init(Default::default)
}
fn online(id: AccountId) -> bool {
    presence().lock().unwrap().contains_key(&id)
}
struct Connected(AccountId);
impl Connected {
    fn new(id: AccountId) -> Self {
        *presence().lock().unwrap().entry(id).or_default() += 1;
        Self(id)
    }
}
impl Drop for Connected {
    fn drop(&mut self) {
        let mut p = presence().lock().unwrap();
        if let Some(count) = p.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                p.remove(&self.0);
            }
        }
    }
}
fn watches() -> &'static Mutex<HashMap<AccountId, watch::Sender<u64>>> {
    static WATCHES: OnceLock<Mutex<HashMap<AccountId, watch::Sender<u64>>>> = OnceLock::new();
    WATCHES.get_or_init(Default::default)
}
fn watch(id: AccountId) -> watch::Receiver<u64> {
    let mut watches = watches().lock().unwrap();
    // Only live sockets retain entries; bounded by the socket limit.
    watches.retain(|_, tx| tx.receiver_count() != 0);
    watches
        .entry(id)
        .or_insert_with(|| watch::channel(0).0)
        .subscribe()
}
/// Call after commit. Wakes these accounts' friends sockets in this process to
/// rebuild their authorized snapshots; carries no data or authorization.
pub(crate) fn changed(accounts: impl IntoIterator<Item = AccountId>) {
    let watches = watches().lock().unwrap();
    for id in accounts {
        if let Some(tx) = watches.get(&id) {
            tx.send_modify(|v| *v = v.wrapping_add(1));
        }
    }
}
async fn upgrade(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(origins): Extension<Arc<Vec<HeaderValue>>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> HttpResponse {
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
            serve(socket, pool).await
        })
}
fn frame(message: Message) -> Option<ClientEvent> {
    let Message::Text(text) = message else {
        return None;
    };
    let frame: ClientFrame = serde_json::from_str(&text).ok()?;
    (frame.version == VERSION).then_some(frame.event)
}
async fn send(socket: &mut WebSocket, event: ServerEvent) -> Result<(), ()> {
    let data = serde_json::to_string(&ServerFrame {
        version: VERSION,
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
async fn serve(mut socket: WebSocket, pool: DbPool) {
    let Ok(Some(Ok(message))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Some(ClientEvent::Connect { token }) = frame(message) else {
        return;
    };
    let p = pool.clone();
    let t = token.clone();
    let Ok(Ok(session)) = crate::performance::blocking(move || {
        let session = auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)?;
        auth::store::rate_limit(
            &p,
            &format!("friend_socket:{}", session.account_id),
            None,
            true,
        )?;
        Ok::<_, Failure>(session)
    })
    .await
    else {
        return;
    };
    let account = crate::chat::access::account(session.account_id);
    let session_scope = crate::chat::access::session(session.id);
    let mut changed = account.subscribe();
    let mut revoked = session_scope.subscribe();
    let _presence = Connected::new(session.account_id);
    let mut selected = None;
    let mut before = None;
    let typing = typing::LeaseOwner::new();
    let mut last = None;
    let mut last_sent = Instant::now();
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0;
    let mut commits = watch(session.account_id);
    // The last database snapshot with the access epochs captured before reading it.
    // Presence and typing are in-memory and refresh on every tick.
    let mut cached = None::<(Snapshot, crate::chat::access::Snapshot)>;
    let mut refresh_at = Instant::now();
    static READERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let readers = READERS.get_or_init(|| Arc::new(Semaphore::new(8)));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {biased;
            _=changed.changed()=>break,
            _=revoked.changed()=>break,
            Ok(())=commits.changed()=>{cached=None;tick.reset_immediately();}
            message=socket.recv()=>{
                let Some(Ok(message))=message else {break;};
                if window.elapsed()>=Duration::from_secs(10) {window=Instant::now();count=0;}
                count+=1;if count>30 {break;}
                last_seen=Instant::now();
                match frame(message) {
                    Some(ClientEvent::Ping {})=>{if send(&mut socket,ServerEvent::Pong {}).await.is_err() {break;}}
                    Some(ClientEvent::Select {conversation_id,before:page})=>{typing.clear();selected=conversation_id;before=page;last=None;cached=None;tick.reset_immediately();}
                    Some(ClientEvent::Typing {active})=>{
                        let Some(id)=selected else {continue;};
                        let Ok(permit)=readers.clone().try_acquire_owned() else {continue;};
                        let p=pool.clone();let t=token.clone();
                        if crate::performance::blocking(move ||{let _permit=permit;store::checked(&p,&t,id,false,|_,_|Ok(()))}).await.is_ok_and(|r|r.is_ok()) {
                            typing.set(id,session.account_id,active);
                        } else {break;}
                    }
                    _=>break,
                }
            }
            _=tick.tick()=>{
                if last_seen.elapsed()>Duration::from_secs(35) {break;}
                if cached.is_none() || Instant::now()>=refresh_at {
                    let Ok(permit)=readers.clone().try_acquire_owned() else {crate::performance::FRIENDS_SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);continue;};
                    // Commits from here on wake the loop again, even during this read.
                    commits.borrow_and_update();
                    let checked=Instant::now();
                    let Some(epoch)=crate::chat::access::snapshot(&account,Some(&session_scope)) else {break;};
                    let p=pool.clone();let t=token.clone();let page=before.clone();
                    let Ok(Ok((snapshot,expiry)))=crate::performance::blocking(move ||{let _permit=permit;store::snapshot(&p,&t,selected,page)}).await else {break;};
                    if last.as_ref()==Some(&snapshot) { crate::performance::FRIENDS_UNCHANGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    refresh_at=checked+expiry.map_or(REFRESH,|e|e.min(REFRESH));
                    cached=Some((snapshot,epoch));
                }
                let (snapshot,epoch)=cached.as_mut().unwrap();
                store::live(snapshot,session.account_id);
                let Some(_delivery)=epoch.clone().deliver() else {break;};
                if last.as_ref()!=Some(&*snapshot) || last_sent.elapsed()>=REFRESH {
                    last_sent=Instant::now();
                    last=Some(snapshot.clone());
                    if send(&mut socket,ServerEvent::Snapshot {snapshot:snapshot.clone()}).await.is_err() {break;}
                }
            }
        }
    }
    let _ = send(&mut socket, ServerEvent::Revoked {}).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presence_combines_devices_and_cleans_up() {
        let id = AccountId::from_uuid(uuid::Uuid::new_v4());
        let a = Connected::new(id);
        let b = Connected::new(id);
        assert!(online(id));
        drop(a);
        assert!(online(id));
        drop(b);
        assert!(!online(id));
    }
    #[test]
    fn commits_wake_only_listed_live_accounts() {
        let id = AccountId::from_uuid(uuid::Uuid::new_v4());
        let other = AccountId::from_uuid(uuid::Uuid::new_v4());
        let mut first = watch(id);
        let mut second = watch(id);
        let mut unrelated = watch(other);
        changed([id]);
        assert!(first.has_changed().unwrap() && second.has_changed().unwrap());
        assert!(!unrelated.has_changed().unwrap());
        first.borrow_and_update();
        second.borrow_and_update();
        unrelated.borrow_and_update();
        drop((first, second));
        // Dropped sockets release their entry on the next registration.
        let _ = watch(AccountId::from_uuid(uuid::Uuid::new_v4()));
        assert!(!watches().lock().unwrap().contains_key(&id));
        changed([id]);
        assert!(!unrelated.has_changed().unwrap());
    }
}
