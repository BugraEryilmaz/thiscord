pub mod auth;
pub mod models;
pub mod utils;
pub mod servers;
pub mod channels;
pub mod websocket;

use tower_sessions_sqlx_store::sqlx::PgPool;
pub use utils::Error;

use axum::{routing::get, Router};
use axum_login::{
    tower_sessions::{self, MemoryStore, SessionManagerLayer}, AuthManagerLayerBuilder
};
use axum_server::tls_rustls::RustlsConfig;
use diesel::{
    PgConnection,
    r2d2::{ConnectionManager, Pool},
};
use std::{net::SocketAddr, path::PathBuf};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub type DbPool = diesel::r2d2::Pool<ConnectionManager<PgConnection>>;

#[tokio::main]
async fn main() {
    // configure tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug,my_web_rtc=info", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Run pending migrations
    // First get the database URL from the environment variable
    dotenvy::dotenv().expect("Failed to load .env file");
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set in the .env file");
    // Then create a connection pool
    let session_pool = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to the database");
    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let db_connection_pool = Pool::builder()
        .build(manager)
        .expect("Failed to create database connection pool");
    // Create a connection to the database
    let mut conn = db_connection_pool
        .get()
        .expect("Failed to get a connection from the pool");
    // Run the migrations
    conn.run_pending_migrations(MIGRATIONS)
        .expect("Failed to run migrations");
    tracing::info!("Migrations completed successfully");
    drop(conn);
    // session manager
    let session_store = tower_sessions_sqlx_store::PostgresStore::new(session_pool.clone());
    session_store.migrate().await.expect("Failed to migrate session store");
    let session_store = tower_sessions::CachingSessionStore::new(MemoryStore::default(), session_store);
    let session_manager_layer = SessionManagerLayer::new(session_store);
    // create auth backend
    let auth_backend = models::Backend::new(db_connection_pool);
    let auth_layer = AuthManagerLayerBuilder::new(auth_backend, session_manager_layer).build();
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install default provider");
    // configure certificate and private key used by https
    let config = RustlsConfig::from_pem_file(
        PathBuf::from("/etc/letsencrypt/live/thiscord.com.tr/fullchain.pem"),
        PathBuf::from("/etc/letsencrypt/live/thiscord.com.tr/privkey.pem"),
    )
    .await
    .unwrap();
    tracing::info!("SSL certificate loaded");

    let static_files = ServeDir::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("images")
    );

    let app = Router::new()
        .route("/", get(handler))
        .nest("/servers", crate::servers::web::router())
        .nest("/auth", crate::auth::web::router())
        .nest("/channels", crate::channels::web::router())
        .nest("/websocket", crate::websocket::web::router())
        .layer(auth_layer)
        .layer(TraceLayer::new_for_http())
        .nest_service("/static", static_files);

    // run https server
    let addr = SocketAddr::from(([0, 0, 0, 0], 443));
    tracing::info!("listening on {}", addr);
    axum_server::bind_rustls(addr, config)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler() -> &'static str {
    "Hello, World!"
}
