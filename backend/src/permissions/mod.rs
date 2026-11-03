pub(crate) mod authorization;
pub mod evaluator;
mod icon;
pub(crate) mod store;

use crate::{
    auth::{self, Failure},
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use thiscord_shared::{RequestId, permissions::*};

pub fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(PERMISSIONS_PATH, post(handle))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route(
            GUILD_ICON_PATH,
            post(icon::set).layer(DefaultBodyLimit::max(MAX_GUILD_ICON_BASE64 + 1024)),
        )
        .route(&format!("{GUILD_ICON_PATH}/{{icon_id}}"), get(icon::get))
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<PermissionRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid permission request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?;
        tokio::task::spawn_blocking(move || {
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

/// Only the local backend CLI exposes initial ownership; never first signup or HTTP.
pub fn bootstrap_owner(pool: &DbPool, username: &str) -> Result<(), crate::BoxError> {
    store::bootstrap(pool, username).map_err(|_| "Owner bootstrap failed: use an existing verified username; the instance must have no owner".into())
}
