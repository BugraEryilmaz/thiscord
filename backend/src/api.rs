use std::time::{Duration, Instant};

use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use thiscord_shared::{
    ApiError, ErrorCode, HEALTH_PATH, HealthResponse, HealthStatus, READY_PATH, REQUEST_ID_HEADER,
    ReadinessResponse, ReadinessStatus, RequestId,
};
use tower_http::cors::CorsLayer;
use tracing::Instrument;
use uuid::Uuid;

use crate::db::{self, DbPool};

pub fn router(pool: Option<DbPool>, origins: Vec<HeaderValue>) -> Router {
    Router::new()
        .route(HEALTH_PATH, get(health))
        .route(READY_PATH, get(ready))
        .merge(crate::auth::router())
        .merge(crate::permissions::router())
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::CONTENT_TYPE,
                ])
                .expose_headers([HeaderName::from_static(REQUEST_ID_HEADER)]),
        )
        // Outermost: even CORS preflight, fallback and error responses get an ID.
        .layer(middleware::from_fn(request_context))
        .with_state(pool)
}

async fn request_context(mut request: Request, next: Next) -> Response {
    // Generate our own ID instead of trusting arbitrary client header values.
    let id = RequestId::from_uuid(Uuid::new_v4());
    let header = HeaderValue::from_str(&id.to_string()).expect("UUID is a valid header");
    request
        .headers_mut()
        .insert(REQUEST_ID_HEADER, header.clone());
    request.extensions_mut().insert(id);
    let span = tracing::info_span!(
        "http_request", request_id = %id, method = %request.method(), path = request.uri().path()
    );
    async move {
        let start = Instant::now();
        let mut response = next.run(request).await;
        response.headers_mut().insert(REQUEST_ID_HEADER, header);
        tracing::info!(
            status = response.status().as_u16(),
            elapsed_ms = start.elapsed().as_millis(),
            "request completed"
        );
        response
    }
    .instrument(span)
    .await
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: HealthStatus::Ok,
    })
}

async fn ready(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let Some(pool) = pool else {
        return unavailable(request_id);
    };
    let probe = tokio::task::spawn_blocking(move || db::check_readiness(&pool));
    match tokio::time::timeout(Duration::from_secs(3), probe).await {
        Ok(Ok(Ok(instance_id))) => Json(ReadinessResponse {
            status: ReadinessStatus::Ready,
            instance_id,
            checked_at: Utc::now(),
        })
        .into_response(),
        // Do not send database details/connection strings to clients or logs.
        Ok(Ok(Err(_))) => {
            tracing::warn!("database readiness query failed");
            unavailable(request_id)
        }
        Ok(Err(_)) => {
            tracing::error!("database readiness task failed");
            unavailable(request_id)
        }
        Err(_) => {
            tracing::warn!("database readiness timed out");
            unavailable(request_id)
        }
    }
}

fn error_response(
    status: StatusCode,
    code: ErrorCode,
    message: &str,
    request_id: RequestId,
) -> Response {
    (
        status,
        Json(ApiError {
            code,
            message: message.into(),
            request_id,
            fields: vec![],
        }),
    )
        .into_response()
}

fn unavailable(request_id: RequestId) -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::ServiceUnavailable,
        "Database is not ready",
        request_id,
    )
}

async fn not_found(Extension(id): Extension<RequestId>) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
        "Route not found",
        id,
    )
}

async fn method_not_allowed(Extension(id): Extension<RequestId>) -> Response {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorCode::MethodNotAllowed,
        "Method not allowed",
        id,
    )
}
