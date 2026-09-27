use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use thiscord_backend::api;
use thiscord_shared::{ApiError, ErrorCode, HEALTH_PATH, READY_PATH, REQUEST_ID_HEADER, RequestId};
use tower::ServiceExt;

#[tokio::test]
async fn liveness_is_independent_and_request_ids_are_server_generated() {
    let app = api::router(None, vec![]);
    let mut ids = vec![];
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(HEALTH_PATH)
                    .header(REQUEST_ID_HEADER, "untrusted-client-value")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .parse::<RequestId>()
            .unwrap();
        ids.push(id);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"status":"ok"})
        );
    }
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn errors_have_stable_codes_and_correlated_request_ids() {
    let app = api::router(None, vec![]);
    for (method, path, status, code) in [
        (
            Method::GET,
            READY_PATH,
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ServiceUnavailable,
        ),
        (
            Method::GET,
            "/missing",
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
        ),
        (
            Method::POST,
            HEALTH_PATH,
            StatusCode::METHOD_NOT_ALLOWED,
            ErrorCode::MethodNotAllowed,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["content-type"], "application/json");
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let error: ApiError = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error.code, code);
        assert_eq!(error.request_id.to_string(), id);
        assert!(error.fields.is_empty());
        assert!(!error.message.contains("postgres"));
    }
}

#[tokio::test]
async fn cors_exposes_correlation_header_and_preflight_also_gets_an_id() {
    let origin = "http://127.0.0.1:1420";
    let app = api::router(None, vec![origin.parse().unwrap()]);
    for method in [Method::GET, Method::OPTIONS] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(HEALTH_PATH)
                    .header("origin", origin)
                    .header("access-control-request-method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["access-control-allow-origin"], origin);
        assert!(response.headers().contains_key(REQUEST_ID_HEADER));
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(READY_PATH)
                .header("origin", origin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers()["access-control-expose-headers"],
        REQUEST_ID_HEADER
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(HEALTH_PATH)
                .header("origin", "https://untrusted.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin")
    );
}
