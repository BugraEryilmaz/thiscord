use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::Engine;
use sha2::Digest;
use tower::ServiceExt;

#[tokio::test]
async fn web_app_bundle_has_scoped_routes_spa_fallback_and_csp() {
    let dir = std::env::temp_dir().join(format!("thiscord-app-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("brand-assets")).unwrap();
    let index = "<!doctype html><script type=\"module\">\r\nimport init from '/app/ui.js';init()\r\n</script>";
    std::fs::write(dir.join("index.html"), index).unwrap();
    std::fs::write(dir.join("ui-abc123_bg.wasm"), b"\0asm").unwrap();
    std::fs::write(dir.join("ui-abc123.js"), "export default 1").unwrap();
    std::fs::write(dir.join("manifest.webmanifest"), "{}").unwrap();
    std::fs::write(dir.join("brand-assets").join("logo.png"), b"\x89PNG").unwrap();
    let app = thiscord_backend::api::router(None, vec![])
        .merge(thiscord_backend::web_app::router(&dir, "https://thiscord.com.tr").unwrap());
    let get = |path: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };
    let expected_hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(
        b"\nimport init from '/app/ui.js';init()\n",
    ));
    for (path, content_type) in [
        ("/app", "text/html"),
        ("/app/", "text/html"),
        ("/app/index.html", "text/html"),
        // Client-side deep links load the app shell.
        ("/app/invite/AbCdEf", "text/html"),
        ("/app/channels/some-id", "text/html"),
        ("/app/ui-abc123_bg.wasm", "application/wasm"),
        ("/app/ui-abc123.js", "javascript"),
        ("/app/manifest.webmanifest", "application/manifest+json"),
        ("/app/brand-assets/logo.png", "image/png"),
    ] {
        let response = get(path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let h = response.headers();
        assert!(
            h["content-type"].to_str().unwrap().contains(content_type),
            "{path}: {:?}",
            h["content-type"]
        );
        let html = content_type == "text/html";
        assert_eq!(
            h["cache-control"],
            if html { "no-store" } else { "no-cache" },
            "{path}"
        );
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(
            h["permissions-policy"],
            "microphone=(self), camera=(self), display-capture=(self)"
        );
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(csp.contains(&format!(
            "script-src 'self' 'wasm-unsafe-eval' 'sha256-{expected_hash}'"
        )));
        assert!(csp.contains("connect-src 'self' wss://thiscord.com.tr;"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("style-src 'self';"));
        assert!(!csp.contains("unsafe-inline"));
        assert!(!csp.contains("ipc") && !csp.contains("localhost"));
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        if html {
            assert_eq!(body, index.as_bytes(), "{path}");
        }
    }
    // Missing assets and traversal attempts are 404s, never the HTML shell;
    // API routes keep their JSON envelope.
    for path in [
        "/app/missing.js",
        "/app/brand-assets/missing.png",
        "/app/../Cargo.toml",
        "/application",
    ] {
        let response = get(path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            !response
                .headers()
                .get("content-type")
                .is_some_and(|v| v.to_str().unwrap().starts_with("text/html")),
            "{path}"
        );
    }
    let response = get("/api/v1/not-a-route").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["content-type"], "application/json");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn web_app_requires_built_bundle_and_valid_public_url() {
    let missing = std::env::temp_dir().join(format!("thiscord-app-{}", uuid::Uuid::new_v4()));
    assert!(thiscord_backend::web_app::router(&missing, "https://thiscord.com.tr").is_err());
    let dir = std::env::temp_dir().join(format!("thiscord-app-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
    assert!(thiscord_backend::web_app::router(&dir, "ftp://thiscord.com.tr").is_err());
    let dev = thiscord_backend::web_app::router(&dir, "http://localhost:3000");
    assert!(dev.is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}
