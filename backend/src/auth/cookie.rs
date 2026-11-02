//! HttpOnly cookie sessions for the same-origin web client (`/app`).
//!
//! Only the request boundary knows about cookies: an allowed cookie becomes the
//! ordinary bearer header, so every existing authorization path stays unchanged.
//! Never log cookie or token values. See docs/web.md.
use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, Method, header},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use thiscord_shared::{
    Timestamp,
    account::{SESSION_MODE_COOKIE, SESSION_MODE_HEADER},
};

/// `__Host-` binds the cookie to this exact origin: Secure, `Path=/`, no `Domain`.
pub const SESSION_COOKIE: &str = "__Host-thiscord_session";
const ATTRIBUTES: &str = "Path=/; HttpOnly; Secure; SameSite=Strict";

/// Request extension: the request's `Origin` header is in `ALLOWED_ORIGINS`.
#[derive(Clone, Copy)]
pub(crate) struct AllowedOrigin;
/// Request extension: the bearer header was derived from the session cookie.
#[derive(Clone, Copy)]
pub(crate) struct CookieSession;

fn origin_allowed(headers: &HeaderMap, origins: &[HeaderValue]) -> bool {
    headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origins.contains(origin))
}

/// Session tokens are 43-character unpadded URL-safe base64 (256 bits).
fn well_formed(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The session cookie value, if exactly one well-formed value is present.
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for pair in value.split(';') {
            let Some((name, token)) = pair.trim().split_once('=') else {
                continue;
            };
            if name == SESSION_COOKIE {
                if found.is_some() {
                    // Ambiguous duplicates are ignored rather than guessed.
                    return None;
                }
                found = Some(token);
            }
        }
    }
    found.filter(|t| well_formed(t)).map(str::to_owned)
}

/// CSRF rule for ambient credentials: an allowed `Origin`, or a safe method
/// without `Origin` (browsers send `Origin` on every cross-origin request and
/// on same-origin non-GET/HEAD requests). `SameSite=Strict` is a second layer.
fn csrf_allows(request: &Request, allowed_origin: bool) -> bool {
    match request.headers().get(header::ORIGIN) {
        Some(_) => allowed_origin,
        None => matches!(*request.method(), Method::GET | Method::HEAD),
    }
}

/// Outer API middleware. An explicit `Authorization` header always wins; a
/// cookie that fails the CSRF rule is ignored (the request is anonymous).
pub(crate) async fn middleware(
    origins: Arc<Vec<HeaderValue>>,
    mut request: Request,
    next: Next,
) -> Response {
    let allowed_origin = origin_allowed(request.headers(), &origins);
    if allowed_origin {
        request.extensions_mut().insert(AllowedOrigin);
    }
    if !request.headers().contains_key(header::AUTHORIZATION)
        && csrf_allows(&request, allowed_origin)
        && let Some(token) = cookie_token(request.headers())
        && let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
        let mut value = value;
        value.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, value);
        request.extensions_mut().insert(CookieSession);
    }
    next.run(request).await
}

/// Whether issued sessions must be delivered as a cookie instead of in the body.
/// Requires an allowed Origin. `rotating_cookie` (rotation of a cookie session)
/// never returns a JavaScript-readable token, even without the mode header.
/// Unauthenticated commands such as login follow only the header, so a same-origin
/// bearer client (the admin dashboard) is unaffected by an ambient web cookie.
pub(crate) fn cookie_mode(
    headers: &HeaderMap,
    allowed_origin: bool,
    rotating_cookie: bool,
) -> bool {
    allowed_origin
        && (rotating_cookie
            || headers
                .get(SESSION_MODE_HEADER)
                .is_some_and(|v| v.as_bytes() == SESSION_MODE_COOKIE.as_bytes()))
}

/// `Set-Cookie` for a newly issued session. Max-Age follows the session's
/// absolute expiry so the browser forgets it no later than the server does.
pub(crate) fn issue(token: &str, expires_at: Timestamp) -> HeaderValue {
    let max_age = (expires_at - chrono::Utc::now()).num_seconds().max(0);
    let mut value = HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={token}; Max-Age={max_age}; {ATTRIBUTES}"
    ))
    .expect("session tokens are header-safe");
    value.set_sensitive(true);
    value
}

pub(crate) fn clear() -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}=; Max-Age=0; {ATTRIBUTES}"))
        .expect("static cookie")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookies: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for c in cookies {
            h.append(header::COOKIE, c.parse().unwrap());
        }
        h
    }

    #[test]
    fn parses_only_one_well_formed_session_cookie() {
        let token = "a".repeat(43);
        assert_eq!(
            cookie_token(&headers(&[&format!("x=1; {SESSION_COOKIE}={token}; y=2")])),
            Some(token.clone())
        );
        assert_eq!(
            cookie_token(&headers(&[&format!("{SESSION_COOKIE}=short")])),
            None
        );
        let bad = format!("{}%", "a".repeat(42));
        assert_eq!(
            cookie_token(&headers(&[&format!("{SESSION_COOKIE}={bad}")])),
            None
        );
        assert_eq!(
            cookie_token(&headers(&[
                &format!("{SESSION_COOKIE}={token}"),
                &format!("{SESSION_COOKIE}={}", "b".repeat(43))
            ])),
            None
        );
        assert_eq!(cookie_token(&headers(&["thiscord_session=x"])), None);
    }

    #[test]
    fn cookie_attributes_bind_to_origin() {
        let expires = chrono::Utc::now() + chrono::Duration::days(30);
        let issued = issue(&"a".repeat(43), expires);
        let issued = issued.to_str().unwrap();
        assert!(issued.starts_with(&format!("{SESSION_COOKIE}=")));
        for attribute in ["Path=/", "HttpOnly", "Secure", "SameSite=Strict"] {
            assert!(issued.contains(attribute), "{attribute}");
        }
        assert!(!issued.to_ascii_lowercase().contains("domain"));
        let age: i64 = issued
            .split("Max-Age=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!((30 * 86_400 - 5..=30 * 86_400).contains(&age));
        assert!(clear().to_str().unwrap().contains("Max-Age=0"));
    }
}
