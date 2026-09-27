//! Account entry points. Database and Argon2 work run on bounded blocking workers.
mod google;
pub mod mail;
mod store;
mod verification;

use crate::db::DbPool;
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{
    net::SocketAddr,
    sync::{Arc, OnceLock},
};
use thiscord_shared::{ApiError, ErrorCode, RequestId, account::*};

#[derive(Debug)]
pub(super) enum Failure {
    Unauthorized,
    Invalid(&'static str),
    Conflict,
    Forbidden,
    Limited,
    Unavailable,
    Database(diesel::result::Error),
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("account operation failed")
    }
}
impl std::error::Error for Failure {}
impl From<diesel::result::Error> for Failure {
    fn from(e: diesel::result::Error) -> Self {
        if matches!(
            e,
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _
            )
        ) {
            Self::Conflict
        } else {
            Self::Database(e)
        }
    }
}
impl Failure {
    fn response(self, request_id: RequestId) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "Invalid credentials, code or expired session",
            ),
            Self::Invalid(m) => (StatusCode::BAD_REQUEST, ErrorCode::ValidationFailed, m),
            Self::Conflict => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "Account or identity is unavailable",
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "Reauthenticate first; keep at least one usable login method",
            ),
            Self::Limited => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "Too many attempts. Try again in one minute",
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::ServiceUnavailable,
                "Account service or provider is unavailable",
            ),
            Self::Database(e) => {
                // Never log database error text, which may contain credentials or submitted values.
                let _ = e;
                tracing::error!("account database operation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::ServiceUnavailable,
                    "Account service is unavailable",
                )
            }
        };
        let mut response = (
            status,
            Json(ApiError {
                code,
                message: message.into(),
                request_id,
                fields: vec![],
            }),
        )
            .into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert("retry-after", "60".parse().unwrap());
        }
        response
    }
}

pub fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(ACCOUNT_PATH, post(handle))
        .route(GOOGLE_CALLBACK_PATH, get(google::callback))
        .route(
            EMAIL_VERIFICATION_PATH,
            get(verification::verify)
                .head(|| async { (StatusCode::NO_CONTENT, [("cache-control", "no-store")]) }),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 43)
        .unwrap_or("")
        .to_owned()
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Result<Json<AccountRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid account request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = bearer(&headers);
        // Proxy headers are deliberately ignored. Configure a trusted proxy before deployment.
        let ip = peer
            .map(|p| p.0.0.ip().to_string())
            .unwrap_or_else(|| "local".into());
        let limit_pool = pool.clone();
        let identifier = match &command {
            AccountRequest::Login { login, .. } => Some(login.trim().to_lowercase()),
            AccountRequest::ForgotPassword { email } => Some(email.trim().to_lowercase()),
            AccountRequest::Register { email, .. } => Some(email.trim().to_lowercase()),
            _ => None,
        };
        let sensitive = !matches!(
            command,
            AccountRequest::Current
                | AccountRequest::Sessions
                | AccountRequest::GoogleComplete { .. }
        );
        tokio::task::spawn_blocking(move || {
            store::rate_limit(&limit_pool, &ip, identifier.as_deref(), sensitive)
        })
        .await
        .map_err(|_| Failure::Unavailable)??;
        match command {
            AccountRequest::GoogleStart {
                purpose,
                callback,
                device,
            } => google::start(pool, token, purpose, callback, device).await,
            command => {
                static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
                let permit = WORKERS
                    .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
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
        }
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
