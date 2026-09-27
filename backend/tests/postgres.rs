use std::{
    env,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use diesel::{connection::SimpleConnection, prelude::*};
use diesel_migrations::MigrationHarness;
use thiscord_backend::{api, db, schema::instance};
use thiscord_shared::{HEALTH_PATH, READY_PATH, ReadinessResponse, Timestamp};
use tower::ServiceExt;
use uuid::Uuid;

struct TestSchema {
    connection: PgConnection,
    name: String,
}

impl Drop for TestSchema {
    fn drop(&mut self) {
        // Identifier consists only of a fixed prefix and UUID hex, never user input.
        let _ = self
            .connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.name));
    }
}

async fn response(app: &Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn migrations_and_readiness_work_against_postgresql() {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    let database_url =
        env::var("TEST_DATABASE_URL").expect("set TEST_DATABASE_URL to a dedicated test database");
    let mut url =
        url::Url::parse(&database_url).expect("TEST_DATABASE_URL must be a PostgreSQL URL");
    assert!(
        url.path().ends_with("_test"),
        "test database name must end in _test"
    );
    let mut schema = TestSchema {
        connection: PgConnection::establish(&database_url).expect("connect to test database"),
        name: format!("foundation_test_{}", Uuid::new_v4().simple()),
    };
    schema
        .connection
        .batch_execute(&format!("CREATE SCHEMA {}", schema.name))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", schema.name));
    let url = url.to_string();
    let pool = tokio::task::spawn_blocking(move || db::connect_and_migrate(&url))
        .await
        .unwrap()
        .unwrap();
    let app = api::router(Some(pool.clone()), vec![]);

    let (first_id, created_at): (Uuid, Timestamp) = instance::table
        .select((instance::id, instance::created_at))
        .first(&mut pool.get().unwrap())
        .unwrap();
    assert!(!first_id.is_nil());
    assert!(created_at.timestamp() > 0);
    assert!(
        pool.get()
            .unwrap()
            .run_pending_migrations(db::MIGRATIONS)
            .unwrap()
            .is_empty()
    );
    assert_eq!(db::check_readiness(&pool).unwrap().as_uuid(), first_id);

    let ready = response(&app, READY_PATH).await;
    assert_eq!(ready.status(), StatusCode::OK);
    let body = to_bytes(ready.into_body(), 4096).await.unwrap();
    let ready: ReadinessResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(ready.instance_id.as_uuid(), first_id);
    assert!(ready.checked_at >= created_at);

    // The migration enforces exactly one possible singleton key.
    assert!(
        diesel::sql_query("INSERT INTO instance(singleton) VALUES (FALSE)")
            .execute(&mut pool.get().unwrap())
            .is_err()
    );
    assert!(
        diesel::sql_query("INSERT INTO instance(singleton) VALUES (TRUE)")
            .execute(&mut pool.get().unwrap())
            .is_err()
    );

    // Verify a stalled/exhausted database path does not stall the HTTP server.
    let leases = (0..5).map(|_| pool.get().unwrap()).collect::<Vec<_>>();
    let started = Instant::now();
    assert_eq!(
        response(&app, READY_PATH).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(response(&app, HEALTH_PATH).await.status(), StatusCode::OK);
    drop(leases);
    assert_eq!(response(&app, READY_PATH).await.status(), StatusCode::OK);

    // Rollback removes the schema dependency, but liveness must still succeed.
    pool.get()
        .unwrap()
        .revert_last_migration(db::MIGRATIONS)
        .unwrap();
    assert_eq!(
        response(&app, READY_PATH).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(response(&app, HEALTH_PATH).await.status(), StatusCode::OK);
    pool.get()
        .unwrap()
        .run_pending_migrations(db::MIGRATIONS)
        .unwrap();
    assert_ne!(db::check_readiness(&pool).unwrap().as_uuid(), first_id);
    assert_eq!(response(&app, READY_PATH).await.status(), StatusCode::OK);
    drop(app);
    drop(pool);
    // TestSchema cleans only its randomly named schema, even on a test panic.
}
