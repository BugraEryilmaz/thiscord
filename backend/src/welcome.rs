//! Public welcome bundle at `/`, with assets isolated under `/welcome/`.
//! Unknown API routes retain their JSON error envelope (no SPA catch-all).
use axum::{Router, extract::Request, middleware, routing::get_service};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};

pub fn router(directory: &Path) -> Result<Router, crate::BoxError> {
    let index = std::fs::read_to_string(directory.join("index.html"))?;
    let mut hashes = String::new();
    // Only Trunk's exact generated bootstrap may run inline.
    for script in index.split("<script").skip(1) {
        let (_, rest) = script
            .split_once('>')
            .ok_or("Invalid welcome HTML script")?;
        let (body, _) = rest
            .split_once("</script>")
            .ok_or("Invalid welcome HTML script")?;
        let body = body.replace("\r\n", "\n").replace('\r', "\n");
        hashes.push_str(&format!(
            " 'sha256-{}'",
            STANDARD.encode(Sha256::digest(body.as_bytes()))
        ));
    }
    let csp = format!("default-src 'none'; script-src 'self' 'wasm-unsafe-eval'{hashes}; style-src 'self'; font-src 'self'; connect-src 'self' https://api.github.com; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'")
        .parse::<axum::http::HeaderValue>()?;
    Ok(Router::new()
        .route(
            "/",
            get_service(ServeFile::new(directory.join("index.html"))),
        )
        .nest_service("/welcome", ServeDir::new(directory))
        .layer(middleware::from_fn(
            move |request: Request, next: middleware::Next| {
                let csp = csp.clone();
                async move {
                    let mut response = next.run(request).await;
                    let h = response.headers_mut();
                    h.insert("content-security-policy", csp);
                    h.insert("cache-control", "no-cache".parse().unwrap());
                    h.insert("x-content-type-options", "nosniff".parse().unwrap());
                    h.insert("referrer-policy", "no-referrer".parse().unwrap());
                    response
                }
            },
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_only_public_bundle_and_preserves_api_errors() {
        let directory =
            std::env::temp_dir().join(format!("thiscord-welcome-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("index.html"),
            "<!doctype html><script type=\"module\">\r\nbootstrap();\r\n</script>",
        )
        .unwrap();
        std::fs::write(directory.join("style.css"), "body{}").unwrap();
        let app = crate::api::router(None, vec![]).merge(router(&directory).unwrap());
        for path in ["/", "/welcome/style.css"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let csp = response.headers()["content-security-policy"]
                .to_str()
                .unwrap();
            assert!(csp.contains("connect-src 'self' https://api.github.com"));
            let digest = STANDARD.encode(Sha256::digest(b"\nbootstrap();\n"));
            assert!(csp.contains(&format!("'sha256-{digest}'")));
            assert!(!csp.contains("'unsafe-inline'"));
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert!(
            response
                .headers()
                .contains_key(thiscord_shared::REQUEST_ID_HEADER)
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/welcome/../Cargo.toml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
