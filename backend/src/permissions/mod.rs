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
    extract::{DefaultBodyLimit, RawQuery, State, rejection::JsonRejection},
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
    RawQuery(query): RawQuery,
    body: Result<Json<PermissionRequest>, JsonRejection>,
) -> Response {
    let set = permission_set(query.as_deref());
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
            store::dispatch(&pool, &token, command, set)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await
    .map(|mut body| {
        body.restrict(set);
        body
    });
    let mut response = match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

/// The client's declared permission set; anything else (including released
/// 0.1.61 clients, which send no query) is set 1. Never an authorization input.
fn permission_set(query: Option<&str>) -> u8 {
    query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == PERMISSION_SET_QUERY)
        .and_then(|(_, value)| value.parse::<u8>().ok())
        .map_or(1, |set| set.clamp(1, PERMISSION_SET))
}

/// Only the local backend CLI exposes initial ownership; never first signup or HTTP.
pub fn bootstrap_owner(pool: &DbPool, username: &str) -> Result<(), crate::BoxError> {
    store::bootstrap(pool, username).map_err(|_| "Owner bootstrap failed: use an existing verified username; the instance must have no owner".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_or_malformed_permission_sets_mean_the_released_set() {
        use super::permission_set;
        assert_eq!(permission_set(None), 1);
        assert_eq!(permission_set(Some("permission_set=x")), 1);
        assert_eq!(permission_set(Some("other=2")), 1);
        assert_eq!(permission_set(Some("permission_set=0")), 1);
        assert_eq!(permission_set(Some("a=1&permission_set=2")), 2);
        assert_eq!(permission_set(Some("permission_set=200")), 2);
    }
}
