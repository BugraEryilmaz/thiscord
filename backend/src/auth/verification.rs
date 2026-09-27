use super::{Failure, store};
use crate::db::DbPool;
use axum::{
    Extension,
    extract::{ConnectInfo, Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use std::net::SocketAddr;
use thiscord_shared::account::{AccountRequest, EMAIL_VERIFICATION_PATH, EmailVerificationQuery};

pub(super) fn link(token: &str) -> Result<String, Failure> {
    let base =
        std::env::var("PUBLIC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:3000".into());
    let mut url = url::Url::parse(&base).map_err(|_| Failure::Unavailable)?;
    if (url.scheme() != "https"
        && !(url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Failure::Unavailable);
    }
    url.set_path(EMAIL_VERIFICATION_PATH);
    url.query_pairs_mut().append_pair("token", token);
    Ok(url.into())
}

pub(super) async fn verify(
    State(pool): State<Option<DbPool>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    input: Result<Query<EmailVerificationQuery>, QueryRejection>,
) -> Response {
    let result = async {
        let Query(input) = input.map_err(|_| Failure::Unauthorized)?;
        if input.token.len() != 43 {
            return Err(Failure::Unauthorized);
        }
        let pool = pool.ok_or(Failure::Unavailable)?;
        let ip = peer
            .map(|p| p.0.0.ip().to_string())
            .unwrap_or_else(|| "local".into());
        tokio::task::spawn_blocking(move || {
            store::rate_limit(&pool, &ip, None, true)?;
            store::dispatch(&pool, "", AccountRequest::VerifyEmail { code: input.token })
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let (status, title, message) = match result {
        Ok(_) => (
            StatusCode::OK,
            "Email verified",
            "Your email has been verified. Return to Thiscord to continue. You can close this tab.",
        ),
        Err(Failure::Unauthorized) => (
            StatusCode::BAD_REQUEST,
            "This link is no longer valid",
            "The link has expired or has already been used. Return to Thiscord to check your verification status or request a new email.",
        ),
        Err(Failure::Limited) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Please try again shortly",
            "Too many attempts. Wait one minute before opening the link again.",
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Verification is temporarily unavailable",
            "Please try opening the same link again shortly.",
        ),
    };
    // All HTML is static text. No scripts, third-party assets, tokens or redirects.
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title} · Thiscord</title></head><body><main><h1>{title}</h1><p>{message}</p></main></body></html>"
    );
    let mut response = (status, Html(html)).into_response();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
        ("x-content-type-options", "nosniff"),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert("retry-after", "60".parse().unwrap());
    }
    response
}
