//! Bounded, process-local push subscriptions. Candidates are hints; every delivery
//! checks current database grants and preferences behind the access barriers.
mod store;
use crate::{
    auth::{self, Failure},
    chat::access,
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
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{AccountId, RequestId, notifications::*};
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct Candidate {
    pub id: String,
    pub target: Target,
    pub kind: Kind,
    pub title: String,
    pub actor: Option<AccountId>,
    pub recipients: Option<Vec<AccountId>>,
    pub mentions: Vec<AccountId>,
    pub call_id: Option<thiscord_shared::ids::CallId>,
    pub version: Option<String>,
}
impl Candidate {
    pub(crate) fn new(target: Target, kind: Kind, title: String, actor: Option<AccountId>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            target,
            kind,
            title,
            actor,
            recipients: None,
            mentions: vec![],
            call_id: None,
            version: None,
        }
    }
    fn release(version: String) -> Self {
        let mut event = Self::new(
            Target::Updates,
            Kind::Update,
            format!("Thiscord {version} is available"),
            None,
        );
        event.id = format!("release:{version}");
        event.version = Some(version);
        event
    }
}
struct Route {
    account: AccountId,
    kinds: Vec<Kind>,
    sender: mpsc::Sender<Candidate>,
}
fn routes() -> &'static Mutex<HashMap<Uuid, Route>> {
    static ROUTES: OnceLock<Mutex<HashMap<Uuid, Route>>> = OnceLock::new();
    ROUTES.get_or_init(Default::default)
}
struct Listener(Uuid);
impl Drop for Listener {
    fn drop(&mut self) {
        routes().lock().unwrap().remove(&self.0);
    }
}

#[cfg(test)]
pub(crate) struct TestListener {
    _guard: Listener,
    pub receiver: mpsc::Receiver<Candidate>,
}
#[cfg(test)]
pub(crate) fn test_listener(account: AccountId) -> TestListener {
    let guard = Listener(Uuid::new_v4());
    let (sender, receiver) = mpsc::channel(64);
    routes().lock().unwrap().insert(
        guard.0,
        Route {
            account,
            kinds: Kind::ALL.to_vec(),
            sender,
        },
    );
    TestListener {
        _guard: guard,
        receiver,
    }
}

pub(crate) fn publish(event: Candidate) {
    routes().lock().unwrap().retain(|_, route| {
        if !route.kinds.contains(&event.kind)
            || event.actor == Some(route.account)
            || event
                .recipients
                .as_ref()
                .is_some_and(|ids| !ids.contains(&route.account))
        {
            return true;
        }
        // A slow subscriber is detached. No unbounded buffers or replay storm.
        route.sender.try_send(event.clone()).is_ok()
    });
}
// Only connected accounts are routing hints. The guild lock held by the send
// transaction stabilizes membership; delivery still checks current channel grants.
pub(crate) fn guild_recipients(
    c: &mut diesel::PgConnection,
    guild: thiscord_shared::GuildId,
) -> Result<Vec<AccountId>, Failure> {
    let accounts: HashSet<_> = routes()
        .lock()
        .unwrap()
        .values()
        .filter(|r| r.kinds.contains(&Kind::GuildMessage))
        .map(|r| r.account)
        .collect();
    if accounts.is_empty() {
        return Ok(vec![]);
    }
    auth::store::query(
        c,
        "SELECT to_jsonb(account_id) AS data FROM guild_members WHERE guild_id=$1::uuid AND account_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))",
        &[
            &guild.to_string(),
            &serde_json::to_string(&accounts).map_err(|_| Failure::Unavailable)?,
        ],
    )
}
pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    Router::new()
        .route(PATH, post(handle))
        .route(SOCKET_PATH, get(upgrade))
        .layer(Extension(Arc::new(origins)))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<Request>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid notification request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let permit = workers()
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
        Ok(prefs) => Json(prefs).into_response(),
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
fn workers() -> Arc<Semaphore> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS.get_or_init(|| Arc::new(Semaphore::new(8))).clone()
}
async fn worker(
    revoked: &mut tokio::sync::watch::Receiver<u64>,
    ended: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<tokio::sync::OwnedSemaphorePermit, ()> {
    // At most one waiter per admitted socket (128 total). FIFO acquisition
    // absorbs normal fanout without detaching healthy clients on contention.
    tokio::select! { biased;
        _ = revoked.changed() => Err(()),
        _ = ended.changed() => Err(()),
        permit = tokio::time::timeout(Duration::from_secs(10), workers().acquire_owned()) => permit.map_err(|_| ())?.map_err(|_| ()),
    }
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
    ws.max_message_size(4096)
        .max_frame_size(4096)
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
    let text = serde_json::to_string(&ServerFrame {
        version: VERSION,
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
async fn serve(mut socket: WebSocket, pool: DbPool) {
    let Ok(Some(Ok(message))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Some(ClientEvent::Connect { token, kinds }) = frame(message) else {
        return;
    };
    if kinds.len() > Kind::ALL.len() {
        return;
    }
    let Ok(Ok(permit)) =
        tokio::time::timeout(Duration::from_secs(10), workers().acquire_owned()).await
    else {
        return;
    };
    let p = pool.clone();
    let t = token.clone();
    let Ok(Ok(session)) = crate::performance::blocking(move || {
        let _permit = permit;
        let session = auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)?;
        auth::store::rate_limit(
            &p,
            &format!("notification_socket:{}", session.account_id),
            None,
            true,
        )?;
        Ok::<_, Failure>(session)
    })
    .await
    else {
        return;
    };
    let account = access::account(session.account_id);
    let session_scope = access::session(session.id);
    let preference_scope = access::notification_preferences(session.account_id);
    let call_scope = access::notification_calls(session.account_id);
    let mut revoked = account.subscribe();
    let mut ended = session_scope.subscribe();
    let mut changed = preference_scope.subscribe();
    let mut calls_changed = call_scope.subscribe();
    let (sender, mut receiver) = mpsc::channel(64);
    let listener = Listener(Uuid::new_v4());
    routes().lock().unwrap().insert(
        listener.0,
        Route {
            account: session.account_id,
            kinds: kinds.clone(),
            sender,
        },
    );
    let mut interests = kinds;
    let mut refresh = true;
    let mut refresh_preferences = true;
    let mut initial = true;
    let mut pending = VecDeque::new();
    let mut delivered_calls = HashSet::new();
    let mut call_deadline = None;
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0;
    let mut maintenance = tokio::time::interval(Duration::from_secs(10));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            _ = revoked.changed() => break,
            _ = ended.changed() => break,
            _ = changed.changed() => { refresh = true; refresh_preferences = true; }
            _ = calls_changed.changed() => { refresh = true; }
            _ = async { match call_deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => { call_deadline = None; refresh = true; }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                if window.elapsed() >= Duration::from_secs(10) { window = Instant::now(); count = 0; } count += 1;
                if count > 30 { break; } last_seen = Instant::now();
                match frame(message) {
                    Some(ClientEvent::Ping {}) => { if send(&mut socket, ServerEvent::Pong {}).await.is_err() { break; } }
                    Some(ClientEvent::Subscribe { kinds }) if kinds.len() <= Kind::ALL.len() => {
                        interests = kinds.clone(); if let Some(route) = routes().lock().unwrap().get_mut(&listener.0) { route.kinds = kinds; }
                        refresh = true;
                        refresh_preferences = true;
                    }
                    _ => break,
                }
            }
            _ = async {}, if refresh => {
                refresh = false;
                let Some(epoch) = access::snapshot(&account, Some(&session_scope)).and_then(|e| e.including(&preference_scope)).and_then(|e| e.including(&call_scope)) else { refresh = true; tokio::task::yield_now().await; continue; };
                let Ok(permit) = worker(&mut revoked, &mut ended).await else { break; };
                let p = pool.clone(); let t = token.clone();
                let Ok(Ok(state)) = crate::performance::blocking(move || { let _permit = permit; store::initial(&p, &t) }).await else { break; };
                let Some(_delivery) = epoch.deliver() else { refresh = true; continue; };
                if refresh_preferences {
                    if send(&mut socket, ServerEvent::Ready { preferences: state.preferences }).await.is_err() { break; }
                    refresh_preferences = false;
                }
                let ringing: Vec<_> = state.calls.iter().map(|c| c.id).collect();
                delivered_calls.retain(|id| ringing.contains(id));
                pending.retain(|c: &Candidate| c.call_id.is_none());
                call_deadline = state.calls.iter().map(|c| {
                    tokio::time::Instant::now() + (c.expires_at - chrono::Utc::now()).to_std().unwrap_or_default()
                }).min();
                if send(&mut socket, ServerEvent::Calls { ringing }).await.is_err() { break; }
                for call in state.calls {
                    if interests.contains(&Kind::IncomingCall) && !delivered_calls.contains(&call.id) {
                        pending.push_back(call.candidate(session.account_id));
                    }
                }
                if initial && let Some(version) = state.release && interests.contains(&Kind::Update) {
                    pending.push_back(Candidate::release(version));
                }
                initial = false;
            }
            candidate = async { if let Some(c) = pending.pop_front() { Some(c) } else { receiver.recv().await } } => {
                let Some(candidate) = candidate else { break; };
                if !interests.contains(&candidate.kind) { continue; }
                let Some(mut epoch) = access::snapshot(&account, Some(&session_scope)).and_then(|e| e.including(&preference_scope)) else { continue; };
                if let Target::Channel { guild_id, .. } = candidate.target {
                    let Some(scoped) = epoch.including(&access::guild(guild_id)).and_then(|e| e.including(&access::member(guild_id, session.account_id))) else { continue; }; epoch = scoped;
                }
                let call_id = candidate.call_id;
                if let Some(id) = call_id {
                    if delivered_calls.contains(&id) { continue; }
                    let Some(scoped) = epoch.including(&call_scope) else { refresh = true; continue; }; epoch = scoped;
                }
                let Ok(permit) = worker(&mut revoked, &mut ended).await else { break; };
                let p = pool.clone(); let t = token.clone();
                let Ok(Ok(alert)) = crate::performance::blocking(move || { let _permit = permit; store::alert(&p, &t, candidate) }).await else { break; };
                let Some(_delivery) = epoch.deliver() else { continue; };
                if let Some(alert) = alert {
                    if send(&mut socket, ServerEvent::Alert { alert }).await.is_err() { break; }
                    if let Some(id) = call_id { delivered_calls.insert(id); }
                }
            }
            _ = maintenance.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) { break; }
                let Ok(permit) = worker(&mut revoked, &mut ended).await else { break; };
                let p = pool.clone(); let t = token.clone();
                if !matches!(crate::performance::blocking(move || { let _permit = permit; auth::store::authenticate(&mut *auth::store::connection(&p)?, &t) }).await, Ok(Ok(_))) { break; }
            }
        }
    }
    let _ = send(&mut socket, ServerEvent::Revoked {}).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fanout_waits_for_workers_and_revocation_cancels_waiting() {
        let held = workers().acquire_many_owned(8).await.unwrap();
        let (revoked, _) = tokio::sync::watch::channel(0);
        let (ended, _) = tokio::sync::watch::channel(0);
        let mut tasks = vec![];
        for _ in 0..16 {
            let mut r = revoked.subscribe();
            let mut e = ended.subscribe();
            tasks.push(tokio::spawn(
                async move { worker(&mut r, &mut e).await.is_ok() },
            ));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(tasks.iter().all(|t| !t.is_finished()));
        drop(held);
        for task in tasks {
            assert!(task.await.unwrap());
        }

        let _held = workers().acquire_many_owned(8).await.unwrap();
        let mut r = revoked.subscribe();
        let mut e = ended.subscribe();
        let task = tokio::spawn(async move { worker(&mut r, &mut e).await.is_err() });
        revoked.send_modify(|v| *v += 1);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
        );
    }

    #[test]
    fn call_transition_fences_checked_but_unsent_invitations() {
        let id = AccountId::from_uuid(Uuid::new_v4());
        let account = access::account(id);
        let calls = access::notification_calls(id);
        let checked = access::snapshot(&account, None)
            .unwrap()
            .including(&calls)
            .unwrap();
        calls.pause().finish(true);
        assert!(checked.deliver().is_none());
        assert!(
            access::snapshot(&account, None)
                .unwrap()
                .including(&calls)
                .unwrap()
                .deliver()
                .is_some()
        );
    }
    #[test]
    fn notifications_bound_backlog_and_exclude_actor_and_unrelated_recipients() {
        let actor = AccountId::from_uuid(Uuid::new_v4());
        let recipient = AccountId::from_uuid(Uuid::new_v4());
        let mut own = test_listener(actor);
        let mut other = test_listener(AccountId::from_uuid(Uuid::new_v4()));
        let mut target = test_listener(recipient);
        let mut event = Candidate::new(Target::Updates, Kind::Update, "test".into(), Some(actor));
        event.recipients = Some(vec![recipient]);
        for _ in 0..1000 {
            publish(event.clone());
        }
        assert!(own.receiver.try_recv().is_err());
        assert!(other.receiver.try_recv().is_err());
        let mut received = 0;
        while target.receiver.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, 64);
        assert!(target.receiver.is_closed());
    }
}
