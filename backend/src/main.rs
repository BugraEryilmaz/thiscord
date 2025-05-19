pub mod auth;
pub mod models;
pub mod schema;

use axum::{routing::get, Extension, Router};
use axum_server::tls_rustls::RustlsConfig;
use diesel::{r2d2::{ConnectionManager, Pool}, PgConnection};
use std::{net::SocketAddr, path::PathBuf};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tower_http::trace::TraceLayer;

use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub type DbPool = diesel::r2d2::Pool<ConnectionManager<PgConnection>>;

#[tokio::main]
async fn main() {
    // configure tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Run pending migrations
    // First get the database URL from the environment variable
    dotenvy::dotenv().expect("Failed to load .env file");
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set in the .env file");
    // Then create a connection pool
    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let db_connection_pool = Pool::builder()
        .build(manager)
        .expect("Failed to create database connection pool");
    // Create a connection to the database
    let mut conn = db_connection_pool.get().expect("Failed to get a connection from the pool");
    // Run the migrations
    conn.run_pending_migrations(MIGRATIONS)
        .expect("Failed to run migrations");
    tracing::info!("Migrations completed successfully");
    drop(conn);

    // configure certificate and private key used by https
    let config = RustlsConfig::from_pem_file(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sslcert")
            .join("cert.pem"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sslcert")
            .join("key.pem"),
    )
    .await
    .unwrap();

    let app = Router::new()
        .route("/", get(handler))
        .layer(Extension(db_connection_pool))
        .layer(TraceLayer::new_for_http());

    // run https server
    let addr = SocketAddr::from(([0, 0, 0, 0], 8081));
    tracing::info!("listening on {}", addr);
    axum_server::bind_rustls(addr, config)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler() -> &'static str {
    "Hello, World!"
}