pub mod auth;
pub mod channels;
pub mod models;
pub mod servers;
pub mod utils;
pub mod websocket;

use std::path::PathBuf;

use axum::Router;
use axum_login::{
    AuthManagerLayerBuilder,
    tower_sessions::{self, MemoryStore, SessionManagerLayer},
};
use diesel::{PgConnection, r2d2::ConnectionManager};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use r2d2::Pool;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tower_sessions_sqlx_store::sqlx::PgPool;
pub use utils::Error;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub fn create_router_with_state(rt: &tokio::runtime::Runtime) -> axum::Router {
    let mut auth_layer = None;
    // Run blocking async code in the main thread
    rt.block_on(async {
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
        session_store
            .migrate()
            .await
            .expect("Failed to migrate session store");
        let session_store =
            tower_sessions::CachingSessionStore::new(MemoryStore::default(), session_store);
        let session_manager_layer = SessionManagerLayer::new(session_store);
        // create auth backend
        let auth_backend = models::Backend::new(db_connection_pool);
        auth_layer =
            Some(AuthManagerLayerBuilder::new(auth_backend, session_manager_layer).build());
    });
    let auth_layer = auth_layer.expect("Failed to create auth layer");

    let static_files = ServeDir::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("images"),
    );

    let app = Router::new()
        .nest("/servers", crate::servers::web::router())
        .nest("/auth", crate::auth::web::router())
        .nest("/channels", crate::channels::web::router())
        .nest("/websocket", crate::websocket::web::router())
        .layer(auth_layer)
        .layer(TraceLayer::new_for_http())
        .nest_service("/static", static_files);
    app
}
