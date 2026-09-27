use std::{env, net::SocketAddr};

use axum::http::HeaderValue;
use thiscord_backend::{BoxError, api, db};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
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

    let args = env::args().skip(1).collect::<Vec<_>>();
    let migrate_only = args == ["--migrate-only"];
    if !args.is_empty() && !migrate_only {
        return Err("usage: thiscord-backend [--migrate-only]".into());
    }

    // A configured database must connect and migrate before serving requests.
    // Without a URL, liveness stays available but readiness returns 503.
    let pool = match env::var("DATABASE_URL") {
        Ok(url) => Some(tokio::task::spawn_blocking(move || db::connect_and_migrate(&url))
            .await?
            .map_err(|_| {
                if cfg!(windows) {
                    "database initialization failed; the local database runs inside WSL. Run ./run-wsl.ps1 from backend/ in PowerShell; see docs/database.md"
                } else {
                    "database initialization failed; check PostgreSQL is running (pg_lsclusters), DATABASE_URL credentials and migrations; see docs/database.md"
                }
            })?),
        Err(env::VarError::NotPresent) if migrate_only => return Err("DATABASE_URL is required for --migrate-only".into()),
        Err(env::VarError::NotPresent) => {
            tracing::warn!("DATABASE_URL is unset; running without a database");
            None
        }
        Err(_) => return Err("DATABASE_URL must contain valid Unicode".into()),
    };

    if migrate_only {
        tracing::info!("database migrations applied");
        return Ok(());
    }
    if let Some(pool) = pool.as_ref() {
        thiscord_backend::auth::mail::start(pool.clone());
    }
    let app = api::router(pool, origins);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "Thiscord backend listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
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
