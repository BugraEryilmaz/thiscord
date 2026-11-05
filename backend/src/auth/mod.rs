//! Account entry points. Database and Argon2 work run on bounded blocking workers.
pub(crate) mod avatar;
pub mod cookie;
mod google;
pub mod mail;
mod password_reset;
pub(crate) mod store;
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
    NotFound,
    Invalid(&'static str),
    Conflict,
    Capacity(&'static str),
    Forbidden,
    Limited,
    Unavailable,
    Configuration(&'static str),
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
    /// Pool exhaustion, timeouts and other database errors say nothing about
    /// authorization. Long-lived connections retry them (delivering nothing
    /// meanwhile) instead of reporting revocation.
    pub(crate) fn transient(&self) -> bool {
        matches!(self, Self::Unavailable | Self::Database(_))
    }
    pub(crate) fn response(self, request_id: RequestId) -> Response {
        let (status, code, message) = match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                "Profile picture not found",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "Invalid credentials, code or expired session",
            ),
            Self::Invalid(m) => (StatusCode::BAD_REQUEST, ErrorCode::ValidationFailed, m),
            Self::Capacity(m) => (StatusCode::CONFLICT, ErrorCode::LimitExceeded, m),
            Self::Conflict => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "Resource conflict; refresh and try again",
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "Operation not permitted; check permissions, ownership and recent reauthentication",
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
            Self::Configuration(setting) => {
                tracing::error!(%request_id, setting, "Invalid or missing account configuration; public account URLs require HTTPS");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::ServiceUnavailable,
                    "Account service or provider is unavailable",
                )
            }
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
        .route(&format!("{AVATAR_PATH}/{{avatar_id}}"), get(avatar::get))
        .route(GOOGLE_CALLBACK_PATH, get(google::callback))
        .route(
            PASSWORD_RESET_PATH,
            get(password_reset::open)
                .head(password_reset::head)
                .post(password_reset::submit)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            EMAIL_VERIFICATION_PATH,
            get(verification::verify)
                .head(|| async { (StatusCode::NO_CONTENT, [("cache-control", "no-store")]) }),
        )
        .layer(DefaultBodyLimit::max(MAX_AVATAR_BASE64 + 1024))
}

pub(crate) fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 43)
        .unwrap_or("")
        .to_owned()
}

/// Socket credentials: a non-empty token in the first frame always wins. The web
/// client sends an empty one and relies on the bearer captured at upgrade time,
/// which the cookie middleware derives from its HttpOnly session cookie.
pub(crate) fn socket_token(frame: String, upgrade: &str) -> String {
    if frame.is_empty() {
        upgrade.to_owned()
    } else {
        frame
    }
}

pub(crate) fn actor_bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 43 || (s.len() == 47 && s.starts_with("bot_")))
        .unwrap_or("")
        .to_owned()
}

// Keep admission ahead of all database work, including rate-limit queries.
fn admit(command: &AccountRequest) -> Result<Arc<tokio::sync::OwnedSemaphorePermit>, Failure> {
    // Provider discovery has its own budget so it cannot exhaust local workers.
    static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    static PROVIDERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let workers = if matches!(command, AccountRequest::GoogleStart { .. }) {
        &PROVIDERS
    } else {
        &WORKERS
    };
    Ok(Arc::new(
        workers
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?,
    ))
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    allowed_origin: Option<Extension<cookie::AllowedOrigin>>,
    cookie_session: Option<Extension<cookie::CookieSession>>,
    headers: HeaderMap,
    body: Result<Json<AccountRequest>, JsonRejection>,
) -> Response {
    let cookie_authenticated = cookie_session.is_some();
    let command = body.as_ref().ok().map(|Json(command)| command);
    let cookie_mode = cookie::cookie_mode(
        &headers,
        allowed_origin.is_some(),
        cookie_authenticated && matches!(command, Some(AccountRequest::Rotate)),
    );
    // Commands that end the current session (or every session) on success.
    let ends_session = matches!(
        command,
        Some(
            AccountRequest::LogoutAll
                | AccountRequest::DeleteAccount { .. }
                | AccountRequest::ChangePassword { .. }
                | AccountRequest::UnlinkIdentity { .. }
        )
    );
    let logout = matches!(command, Some(AccountRequest::Logout));
    // Only pure session checks clear on 401: a mistyped reauthentication
    // password must not sign the browser out.
    let session_check = matches!(
        command,
        Some(AccountRequest::Current | AccountRequest::Sessions | AccountRequest::Rotate)
    );
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid account request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = bearer(&headers);
        let permit = admit(&command)?;
        let limit_permit = permit.clone();
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
            let _permit = limit_permit;
            store::rate_limit(&limit_pool, &ip, identifier.as_deref(), sensitive)
        })
        .await
        .map_err(|_| Failure::Unavailable)??;
        match command {
            AccountRequest::GoogleStart {
                purpose,
                callback,
                device,
            } => google::start(pool, token, purpose, callback, device, permit).await,
            command => tokio::task::spawn_blocking(move || {
                let _permit = permit;
                store::dispatch(&pool, &token, command)
            })
            .await
            .map_err(|_| Failure::Unavailable)?,
        }
    }
    .await;
    let clear = cookie_authenticated
        && (logout
            || (ends_session && result.is_ok())
            || (session_check && matches!(result, Err(Failure::Unauthorized))));
    let mut set_cookie = None;
    let mut response = match result {
        // Web client sessions stay out of JavaScript: deliver only the cookie.
        Ok(AccountResponse::Session { mut session }) if cookie_mode => {
            set_cookie = Some(cookie::issue(&session.token, session.expires_at));
            session.token = String::new();
            Json(AccountResponse::Session { session }).into_response()
        }
        Ok(body) => Json(body).into_response(),
        Err(e) => e.response(id),
    };
    if let Some(value) = set_cookie.or_else(|| clear.then(cookie::clear)) {
        response.headers_mut().insert("set-cookie", value);
    }
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_provider_budget_does_not_exhaust_local_account_admission() {
        let discovery = AccountRequest::GoogleStart {
            purpose: GooglePurpose::Login,
            callback: None,
            device: "test".into(),
        };
        let providers: Vec<_> = (0..4).map(|_| admit(&discovery).unwrap()).collect();
        assert!(matches!(admit(&discovery), Err(Failure::Limited)));
        let local = admit(&AccountRequest::Current).unwrap();
        drop(providers);
        assert!(admit(&discovery).is_ok());
        drop(local);
    }
}
