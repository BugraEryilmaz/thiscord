//! Browser web client bundle at `/app` (see docs/web.md). Serves only the
//! explicitly built bundle, never the source tree. Unknown extensionless paths
//! fall back to `index.html` so client-side deep links load the app; missing
//! assets and all API routes keep their own 404s.
use axum::{
    Router,
    body::Body,
    extract::Request,
    handler::HandlerWithoutStateExt,
    http::{HeaderValue, StatusCode, Uri, header},
    middleware,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiscord_shared::account::WEB_APP_PATH;
use tower_http::services::ServeDir;

/// WebSocket origin matching the public HTTP origin. CSP Level 3 `'self'` also
/// covers same-host `wss:`, but older WebKit releases require it explicitly.
fn socket_origin(public_url: &str) -> Result<String, crate::BoxError> {
    let url = url::Url::parse(public_url).map_err(|_| "PUBLIC_BACKEND_URL must be a URL")?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err("PUBLIC_BACKEND_URL must use http or https".into()),
    };
    let host = url
        .host_str()
        .ok_or("PUBLIC_BACKEND_URL must contain a host")?;
    Ok(match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

pub fn router(directory: &Path, public_url: &str) -> Result<Router, crate::BoxError> {
    let index = std::fs::read_to_string(directory.join("index.html"))?;
    // Hash Trunk's exact inline module bootstrap instead of allowing arbitrary JS.
    let mut hashes = String::new();
    for script in index.split("<script").skip(1) {
        let (_, rest) = script.split_once('>').ok_or("Invalid app HTML script")?;
        let (body, _) = rest
            .split_once("</script>")
            .ok_or("Invalid app HTML script")?;
        // HTML parsing normalizes Windows and old-Mac newlines before CSP hashing.
        let body = body.replace("\r\n", "\n").replace('\r', "\n");
        hashes.push_str(&format!(
            " 'sha256-{}'",
            STANDARD.encode(Sha256::digest(body.as_bytes()))
        ));
    }
    let socket = socket_origin(public_url)?;
    // Desktop CSP minus Tauri IPC/asset/localhost entries. The web client only
    // talks to this origin; no third-party scripts, styles, fonts or frames.
    let csp = format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{hashes}; style-src 'self'; \
         img-src 'self' blob: data:; media-src 'self' blob:; connect-src 'self' {socket}; \
         object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
    )
    .parse::<HeaderValue>()?;
    let index = axum::body::Bytes::from(index);
    let spa = move |uri: Uri| {
        let index = index.clone();
        async move {
            let last = uri.path().rsplit('/').next().unwrap_or("");
            // Asset-like names (with an extension) must not resolve to HTML.
            if last.contains('.') {
                StatusCode::NOT_FOUND.into_response()
            } else {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    Body::from(index),
                )
                    .into_response()
            }
        }
    };
    let files = ServeDir::new(directory)
        .append_index_html_on_directories(false)
        .fallback(spa.into_service());
    Ok(Router::new()
        .nest_service(WEB_APP_PATH, files)
        .layer(middleware::from_fn(
            move |request: Request, next: middleware::Next| {
                let csp = csp.clone();
                async move {
                    let mut response: Response = next.run(request).await;
                    let html = response
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.starts_with("text/html"));
                    let h = response.headers_mut();
                    h.insert("content-security-policy", csp);
                    // HTML names the current hashed assets and must never be
                    // stale; other files revalidate (Last-Modified) on use.
                    h.insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static(if html { "no-store" } else { "no-cache" }),
                    );
                    h.insert(
                        "x-content-type-options",
                        HeaderValue::from_static("nosniff"),
                    );
                    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
                    h.insert(
                        "permissions-policy",
                        HeaderValue::from_static(
                            "microphone=(self), camera=(self), display-capture=(self)",
                        ),
                    );
                    h.insert(
                        "cross-origin-opener-policy",
                        HeaderValue::from_static("same-origin"),
                    );
                    response
                }
            },
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_origin_follows_public_scheme_and_port() {
        assert_eq!(
            socket_origin("https://thiscord.com.tr").unwrap(),
            "wss://thiscord.com.tr"
        );
        assert_eq!(
            socket_origin("http://localhost:3000/").unwrap(),
            "ws://localhost:3000"
        );
        assert!(socket_origin("ftp://example.test").is_err());
        assert!(socket_origin("not a url").is_err());
    }
}
