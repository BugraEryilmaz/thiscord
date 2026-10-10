//! Serve only the explicitly built public bundle, never the source tree.
use axum::{Router, extract::Request, middleware, response::Response};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::path::Path;
use tower_http::services::ServeDir;

pub fn router(directory: &Path) -> Result<Router, crate::BoxError> {
    let index = std::fs::read_to_string(directory.join("index.html"))?;
    // Hash Trunk's exact inline module bootstrap instead of allowing arbitrary JS.
    let mut hashes = String::new();
    for script in index.split("<script").skip(1) {
        let (_, rest) = script.split_once('>').ok_or("Invalid admin HTML script")?;
        let (body, _) = rest
            .split_once("</script>")
            .ok_or("Invalid admin HTML script")?;
        // HTML parsing normalizes Windows and old-Mac newlines before CSP hashing.
        let body = body.replace("\r\n", "\n").replace('\r', "\n");
        hashes.push_str(&format!(
            " 'sha256-{}'",
            STANDARD.encode(Sha256::digest(body.as_bytes()))
        ));
    }
    let csp = format!("default-src 'none'; script-src 'self' 'wasm-unsafe-eval'{hashes}; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'").parse::<axum::http::HeaderValue>()?;
    Ok(Router::new()
        .nest_service("/admin", ServeDir::new(directory))
        .layer(middleware::from_fn(
            move |request: Request, next: middleware::Next| {
                let csp = csp.clone();
                async move {
                    let mut response: Response = next.run(request).await;
                    let h = response.headers_mut();
                    h.insert("content-security-policy", csp);
                    h.insert("cache-control", "no-store".parse().unwrap());
                    h.insert("x-content-type-options", "nosniff".parse().unwrap());
                    h.insert("referrer-policy", "no-referrer".parse().unwrap());
                    response
                }
            },
        )))
}
