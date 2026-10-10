use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn admin_bundle_has_scoped_routes_and_csp() {
    let dir = std::env::temp_dir().join(format!("thiscord-admin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><script type=\"module\">\r\nconsole.log('bootstrap')\r\n</script>",
    )
    .unwrap();
    std::fs::write(dir.join("app.wasm"), b"\0asm").unwrap();
    let app = thiscord_backend::admin::web::router(&dir).unwrap();
    for path in ["/admin", "/admin/", "/admin/index.html", "/admin/app.wasm"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        let csp = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        use base64::Engine;
        use sha2::Digest;
        let expected = base64::engine::general_purpose::STANDARD
            .encode(sha2::Sha256::digest(b"\nconsole.log('bootstrap')\n"));
        assert!(csp.contains(&format!("'sha256-{expected}'")));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(
            !csp.split("style-src")
                .next()
                .unwrap()
                .contains("'unsafe-inline'")
        );
        if path.ends_with("wasm") {
            assert_eq!(response.headers()["content-type"], "application/wasm");
        }
        assert!(
            !to_bytes(response.into_body(), 8192)
                .await
                .unwrap()
                .is_empty()
        );
    }
    for path in ["/admin/missing", "/admin/../Cargo.toml", "/api/v1/health"] {
        assert_eq!(
            app.clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    std::fs::remove_file(dir.join("index.html")).unwrap();
    std::fs::remove_file(dir.join("app.wasm")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
