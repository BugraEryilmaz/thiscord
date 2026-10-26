//! Bounded request/reply relay. Connection identity, never account name, selects a desktop.
use crate::{
    auth::{self, Failure, store},
    chat::access,
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use diesel::Connection;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{RequestId, permissions::InstanceRole, remote_audio::*};
use tokio::sync::{Semaphore, mpsc, oneshot};

struct Target {
    calls: mpsc::Sender<Call>,
    owner_only: bool,
    video_diagnostics: bool,
}
type Registry = Mutex<HashMap<RequestId, Target>>;
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
pub(crate) struct Registration(RequestId);
impl Drop for Registration {
    fn drop(&mut self) {
        registry().lock().unwrap().remove(&self.0);
    }
}
pub(crate) fn register(
    id: RequestId,
    owner_only: bool,
    video_diagnostics: bool,
) -> (Registration, mpsc::Receiver<Call>) {
    let (calls, rx) = mpsc::channel(1);
    registry().lock().unwrap().insert(
        id,
        Target {
            calls,
            owner_only,
            video_diagnostics,
        },
    );
    (Registration(id), rx)
}
pub(crate) fn available(id: RequestId) -> bool {
    registry().lock().unwrap().contains_key(&id)
}

pub(crate) struct Call {
    pub id: RequestId,
    pub command: Command,
    pub authorized: access::Snapshot,
    reply: oneshot::Sender<Result<String, Failure>>,
    started: Instant,
    bytes: String,
}
impl Call {
    pub fn expired(&self) -> bool {
        self.reply.is_closed() || self.started.elapsed() > Duration::from_secs(60)
    }
    pub fn reject(self, error: Failure) {
        let _ = self.reply.send(Err(error));
    }
    pub fn chunk(&mut self, chunk: &str) -> Result<(), Failure> {
        if chunk.len() > CHUNK_BYTES || self.bytes.len() + chunk.len() > MAX_REPLY_BYTES {
            return Err(Failure::Invalid("Invalid audio reply size"));
        }
        self.bytes.push_str(chunk);
        Ok(())
    }
    pub fn finish(self) {
        let _ = self.reply.send(Ok(self.bytes));
    }
}

pub(super) fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(PATH, post(handle))
        .layer(DefaultBodyLimit::max(32 * 1024))
}

async fn authorize(
    pool: DbPool,
    token: String,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(access::Snapshot, InstanceRole), Failure> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut c = store::connection(&pool)?;
        let identity = store::token_session(&mut c, &token)?;
        let authorized = access::snapshot(
            &access::account(identity.account_id),
            Some(&access::instance_role(identity.account_id)),
        )
        .and_then(|s| s.including(&access::session(identity.id)))
        .ok_or(Failure::Forbidden)?;
        let session = store::authenticate(&mut c, &token)?;
        let role = c.transaction::<_, Failure, _>(|c| {
            store::read_session(c, &token, &session)?;
            let role = crate::permissions::store::instance(c, session.account_id)?.role;
            if !matches!(role, InstanceRole::Owner | InstanceRole::Admin) {
                return Err(Failure::Forbidden);
            }
            Ok(role)
        })?;
        Ok((authorized, role))
    })
    .await
    .map_err(|_| Failure::Unavailable)?
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<Request>, JsonRejection>,
) -> Response {
    let result = async {
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = Arc::new(
            WORKERS
                .get_or_init(|| Arc::new(Semaphore::new(8)))
                .clone()
                .try_acquire_owned()
                .map_err(|_| Failure::Limited)?,
        );
        let (authorized, role) = authorize(
            pool.ok_or(Failure::Unavailable)?,
            auth::bearer(&headers),
            permit.clone(),
        )
        .await?;
        let Json(request) = body.map_err(|_| Failure::Invalid("Invalid remote audio request"))?;
        request.command.validate().map_err(Failure::Invalid)?;
        let (sender, owner_only, video_diagnostics) = registry()
            .lock()
            .unwrap()
            .get(&request.connection_id)
            .map(|t| (t.calls.clone(), t.owner_only, t.video_diagnostics))
            .ok_or(Failure::Conflict)?;
        if owner_only && role != InstanceRole::Owner {
            return Err(Failure::Forbidden);
        }
        // Older desktops end their voice connection on unknown commands.
        if matches!(request.command, Command::VideoDiagnostics {}) && !video_diagnostics {
            return Err(Failure::Invalid(
                "Video diagnostics need an updated desktop app. Ask the user to update and rejoin voice.",
            ));
        }
        let (reply, response) = oneshot::channel();
        sender
            .try_send(Call {
                id,
                command: request.command,
                authorized: authorized.clone(),
                reply,
                started: Instant::now(),
                bytes: String::new(),
            })
            .map_err(|_| Failure::Limited)?;
        let reply = tokio::time::timeout(Duration::from_secs(60), response)
            .await
            .map_err(|_| Failure::Unavailable)?
            .map_err(|_| Failure::Conflict)??;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let reply: Reply = serde_json::from_str(&reply)
                .map_err(|_| Failure::Invalid("Invalid desktop audio reply"))?;
            let _delivery = authorized.deliver().ok_or(Failure::Forbidden)?;
            Ok::<_, Failure>(Json(reply).into_response())
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let mut response = result.unwrap_or_else(|e| e.response(id));
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call() -> (Call, oneshot::Receiver<Result<String, Failure>>) {
        let (reply, response) = oneshot::channel();
        let account = thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4());
        (
            Call {
                id: RequestId::from_uuid(uuid::Uuid::new_v4()),
                command: Command::AudioCurrent {},
                authorized: access::snapshot(&access::account(account), None).unwrap(),
                reply,
                started: Instant::now(),
                bytes: String::new(),
            },
            response,
        )
    }
    #[tokio::test]
    async fn chunks_are_bounded_and_cancelled_calls_expire() {
        let (mut call, response) = call();
        assert!(call.chunk(&"x".repeat(CHUNK_BYTES + 1)).is_err());
        for _ in 0..MAX_REPLY_BYTES / CHUNK_BYTES {
            call.chunk(&"x".repeat(CHUNK_BYTES)).unwrap();
        }
        assert!(call.chunk("x").is_err());
        assert!(!call.expired());
        drop(response);
        assert!(call.expired());
    }
    #[tokio::test]
    async fn replies_reassemble_without_exposing_a_partial_result() {
        let (mut call, mut response) = call();
        call.chunk(r#"{"Ok":"hel"#).unwrap();
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        call.chunk(r#"lo"}"#).unwrap();
        call.finish();
        assert_eq!(response.await.unwrap().unwrap(), r#"{"Ok":"hello"}"#);
    }
}
