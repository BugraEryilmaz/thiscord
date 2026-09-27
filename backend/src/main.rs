mod db;
mod schema;

use std::{env, error::Error, net::SocketAddr};

use axum::{
    Json, Router,
    http::{HeaderValue, Method},
    routing::get,
};
use thiscord_shared::{HEALTH_PATH, HealthResponse, HealthStatus};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let bind: SocketAddr = env::var("BACKEND_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;
    let origins = env::var("ALLOWED_ORIGINS")
        .unwrap_or_else(|_| {
            "http://localhost:1420,http://127.0.0.1:1420,http://tauri.localhost,tauri://localhost"
                .into()
        })
        .split(',')
        .map(|origin| origin.trim().parse::<HeaderValue>())
        .collect::<Result<Vec<_>, _>>()?;

    // Startup fails if a configured database is unavailable. Without a URL,
    // the skeleton serves only liveness and can run before PostgreSQL setup.
    let pool = match env::var("DATABASE_URL") {
        Ok(url) => Some(tokio::task::spawn_blocking(move || db::connect(&url)).await??),
        Err(env::VarError::NotPresent) => {
            tracing::warn!("DATABASE_URL is unset; running without a database");
            None
        }
        Err(error) => return Err(error.into()),
    };

    let app = Router::new()
        .route(HEALTH_PATH, get(health))
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET]),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(pool);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "Thiscord backend listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: HealthStatus::Ok,
    })
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install Ctrl-C handler");
}
