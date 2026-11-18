//! Email verification links. A link is consumed only by a real top-level browser
//! navigation or by the confirmation form; plain fetchers (mail gateways, link
//! scanners) see the form and change nothing. See docs/accounts.md.
use super::{
    Failure,
    password_reset::{secure, valid_token},
    store,
};
use crate::db::DbPool;
use axum::{
    Extension, Form,
    extract::{
        ConnectInfo, Query, State,
        rejection::{FormRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use std::net::SocketAddr;
use thiscord_shared::account::{
    AccountRequest, AccountResponse, EMAIL_VERIFICATION_PATH, EmailVerificationQuery,
};

pub(super) fn link(token: &str) -> Result<String, Failure> {
    account_link(EMAIL_VERIFICATION_PATH, token)
}

pub(super) fn account_link(path: &str, token: &str) -> Result<String, Failure> {
    let base =
        std::env::var("PUBLIC_BACKEND_URL").unwrap_or_else(|_| "http://localhost:3000".into());
    let mut url =
        url::Url::parse(&base).map_err(|_| Failure::Configuration("PUBLIC_BACKEND_URL"))?;
    if (url.scheme() != "https"
        && !(url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Failure::Configuration("PUBLIC_BACKEND_URL"));
    }
    url.set_path(path);
    url.query_pairs_mut().append_pair("token", token);
    Ok(url.into())
}

/// Fetch Metadata of a top-level document navigation that is not a speculative
/// prefetch/prerender. Browsers send it only to HTTPS and localhost origins,
/// which are the only origins `account_link` builds. Neither `Sec-Fetch-User`
/// nor `Sec-Fetch-Site` is required: links opened from mail apps and webmail differ.
fn navigation(headers: &HeaderMap) -> bool {
    let is = |name: &str, expected: &str| {
        let mut values = headers.get_all(name).iter();
        values
            .next()
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(expected.as_bytes()))
            && values.next().is_none()
    };
    let speculative = ["sec-purpose", "purpose", "x-moz"].iter().any(|name| {
        headers.get_all(*name).iter().any(|value| {
            let value = value.as_bytes().to_ascii_lowercase();
            value.windows(8).any(|w| w == b"prefetch")
                || value.windows(9).any(|w| w == b"prerender")
        })
    });
    is("sec-fetch-mode", "navigate") && is("sec-fetch-dest", "document") && !speculative
}

pub(super) async fn open(
    State(pool): State<Option<DbPool>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    input: Result<Query<EmailVerificationQuery>, QueryRejection>,
) -> Response {
    let token = match input {
        Ok(Query(input)) if valid_token(&input.token) => input.token,
        _ => return outcome(Err(Failure::Unauthorized)),
    };
    if navigation(&headers) {
        return outcome(consume(pool, peer, &headers, token).await);
    }
    // Anything else (no Fetch Metadata, a subresource or a prefetch) leaves the
    // token unused and offers the same operation as an explicit form submission.
    page(
        StatusCode::OK,
        "Confirm your email address",
        "Select the button below to finish confirming this email address for your Thiscord account.",
        &format!(
            r#"
        <form method="post" action="{EMAIL_VERIFICATION_PATH}">
            <input type="hidden" name="token" value="{token}">
            <button type="submit" autofocus>Confirm email address</button>
        </form>"#
        ),
    )
}

/// The fallback form. The token in the body is the only capability; no cookie or
/// session is read, so a cross-site submission can do no more than the link can.
pub(super) async fn submit(
    State(pool): State<Option<DbPool>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    input: Result<Form<EmailVerificationQuery>, FormRejection>,
) -> Response {
    match input {
        Ok(Form(input)) if valid_token(&input.token) => {
            outcome(consume(pool, peer, &headers, input.token).await)
        }
        _ => outcome(Err(Failure::Unauthorized)),
    }
}

async fn consume(
    pool: Option<DbPool>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &HeaderMap,
    token: String,
) -> Result<AccountResponse, Failure> {
    let pool = pool.ok_or(Failure::Unavailable)?;
    let ip = crate::client_ip::limit_key(peer.map(|p| p.0.0), headers);
    tokio::task::spawn_blocking(move || {
        store::rate_limit(&pool, &ip, None, true)?;
        store::dispatch(&pool, "", AccountRequest::VerifyEmail { code: token })
    })
    .await
    .map_err(|_| Failure::Unavailable)?
}

fn outcome(result: Result<AccountResponse, Failure>) -> Response {
    let (status, title, message) = match result {
        Ok(AccountResponse::Done { message }) if message == "Email changed" => (
            StatusCode::OK,
            "Email address changed",
            "Your new email address is confirmed. Use it to sign in to Thiscord from now on. You can close this tab.",
        ),
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
        // A concurrent claim after the availability check fails as a conflict.
        Err(Failure::Invalid(_) | Failure::Conflict) => (
            StatusCode::CONFLICT,
            "This address is already in use",
            "Another account started using this email address. Open Settings in Thiscord to choose a different one.",
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
    let mut response = page(status, title, message, "");
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert("retry-after", "60".parse().unwrap());
    }
    response
}

/// Only static text and a validated URL-safe token enter the page. No scripts,
/// third-party assets or redirects; the shared account-page CSP applies.
fn page(status: StatusCode, title: &str, message: &str, form: &str) -> Response {
    let html = include_str!("password_reset.html")
        .replace("{{title}}", title)
        .replace("{{message}}", message)
        .replace("{{form}}", form)
        .replace("{{support}}", "");
    secure((status, Html(html)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn only_top_level_non_speculative_navigations_consume_links() {
        let link = [
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "document"),
        ];
        // Mail apps (site none) and webmail (cross-site), with or without a user gesture.
        assert!(navigation(&headers(&link)));
        assert!(navigation(&headers(&[
            ("sec-fetch-mode", "Navigate"),
            ("sec-fetch-dest", "document"),
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-user", "?1"),
        ])));
        for refused in [
            &[][..],
            &[("sec-fetch-mode", "navigate")][..],
            &[
                ("sec-fetch-mode", "no-cors"),
                ("sec-fetch-dest", "document"),
            ][..],
            &[("sec-fetch-mode", "navigate"), ("sec-fetch-dest", "iframe")][..],
            &[("sec-fetch-mode", "navigate"), ("sec-fetch-dest", "empty")][..],
            &[
                ("sec-fetch-mode", "navigate"),
                ("sec-fetch-mode", "navigate"),
                ("sec-fetch-dest", "document"),
            ][..],
        ] {
            assert!(!navigation(&headers(refused)), "{refused:?}");
        }
        for purpose in [
            ("sec-purpose", "prefetch"),
            ("sec-purpose", "prefetch;prerender"),
            ("sec-purpose", "prefetch;anonymous-client-ip"),
            ("purpose", "prefetch"),
            ("purpose", "Prerender"),
            ("x-moz", "prefetch"),
        ] {
            let mut map = headers(&link);
            map.append(purpose.0, purpose.1.parse().unwrap());
            assert!(!navigation(&map), "{purpose:?}");
        }
    }
}
