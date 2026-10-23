//! Guild-scoped scripting principals and live mention subscriptions.
mod socket;
mod store;
use crate::{
    auth::{self, Failure},
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use diesel::PgConnection;
use std::sync::{Arc, OnceLock};
use thiscord_shared::{RequestId, bots::*, chat::ChatMessage};
use tokio::sync::{Semaphore, broadcast};

pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    Router::new()
        .route(BOTS_PATH, post(handle))
        .route(CHANNELS_PATH, post(channels))
        .route(SOCKET_PATH, get(socket::upgrade))
        .layer(Extension(Arc::new(origins)))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
fn events() -> &'static broadcast::Sender<ChatMessage> {
    static EVENTS: OnceLock<broadcast::Sender<ChatMessage>> = OnceLock::new();
    EVENTS.get_or_init(|| broadcast::channel(256).0)
}
/// Called by the blocking chat writer only after a new human send commits.
pub(crate) fn publish(message: ChatMessage) {
    let _ = events().send(message);
}

pub(crate) fn authenticate(
    c: &mut PgConnection,
    token: &str,
) -> Result<auth::store::Session, Failure> {
    if token.len() != 47 || !token.starts_with("bot_") {
        return Err(Failure::Unauthorized);
    }
    // Hash the full prefixed credential. Removing its prefix never produces a
    // stored user token. Membership removal immediately disables the credential.
    auth::store::query(c, "SELECT to_jsonb(s)||jsonb_build_object('bot_guild',b.guild_id) AS data FROM sessions s JOIN session_tokens t ON t.session_id=s.id JOIN scripting_bots b ON b.account_id=s.account_id JOIN accounts a ON a.id=b.account_id JOIN guild_members m ON m.guild_id=b.guild_id AND m.account_id=b.account_id WHERE t.token_hash=$1 AND t.active AND NOT s.revoked AND s.expires_at>now() AND a.is_bot", &[&auth::store::digest(token)])?.pop().ok_or(Failure::Unauthorized)
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<BotRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid bot request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
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
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

async fn channels(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<ChannelsRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(_) = body.map_err(|_| Failure::Invalid("Invalid channel request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::actor_bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?;
        crate::performance::blocking(move || {
            let _permit = permit;
            store::channels(&pool, &token)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let mut response = match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
