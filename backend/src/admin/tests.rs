use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use diesel::{PgConnection, connection::SimpleConnection};
use serde_json::{Value, json};
use thiscord_shared::{AccountId, account::ACCOUNT_PATH, permissions::PERMISSIONS_PATH};
use tokio::sync::oneshot;
use tower::ServiceExt;
use uuid::Uuid;

struct Database {
    connection: PgConnection,
    schema: String,
    pool: DbPool,
}
impl Drop for Database {
    fn drop(&mut self) {
        self.connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .unwrap();
    }
}
fn database() -> Database {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("admin_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = crate::db::connect_and_migrate(url.as_str()).unwrap();
    Database {
        connection,
        schema,
        pool,
    }
}
fn user(db: &Database, name: &str) -> (AccountId, String) {
    let id = AccountId::from_uuid(Uuid::new_v4());
    let session = Uuid::new_v4().to_string();
    let token = store::digest(&Uuid::new_v4().to_string());
    let mut c = db.pool.get().unwrap();
    store::execute(&mut c, "INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)", &[&id.to_string(),name]).unwrap();
    store::execute(&mut c, "INSERT INTO sessions(id,account_id,device,reauthenticated_at) VALUES($1::uuid,$2::uuid,'test',now())", &[&session,&id.to_string()]).unwrap();
    store::execute(
        &mut c,
        "INSERT INTO session_tokens(token_hash,session_id) VALUES($1,$2::uuid)",
        &[&store::digest(&token), &session],
    )
    .unwrap();
    (id, token)
}
fn headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    headers
}
async fn assert_response(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap();
    if !status.is_success() {
        assert!(value.get("rooms").is_none());
        assert!(value.get("host").is_none());
        assert!(value["request_id"].is_string());
    }
    value
}
async fn command(app: &Router, token: &str, path: &str, body: Value, status: StatusCode) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.clone().oneshot(request),
    )
    .await
    .expect("diagnostics collection blocked an access mutation")
    .unwrap();
    assert_response(response, status).await;
}
async fn get(app: &Router, token: &str, status: StatusCode) -> Value {
    let request = Request::builder()
        .uri(DIAGNOSTICS_PATH)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_response(app.clone().oneshot(request).await.unwrap(), status).await
}
// Pause exactly after the authorization transaction, before telemetry completes.
async fn collecting(
    db: &Database,
    token: &str,
) -> (oneshot::Sender<()>, tokio::task::JoinHandle<Response>) {
    let (ready, started) = oneshot::channel();
    let (release, proceed) = oneshot::channel();
    let pool = db.pool.clone();
    let headers = headers(token);
    let task = tokio::spawn(respond(
        Some(pool),
        RequestId::from_uuid(Uuid::new_v4()),
        Instant::now(),
        headers,
        async move {
            ready.send(()).unwrap();
            proceed.await.unwrap();
            HashMap::new()
        },
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    (release, task)
}
async fn finish(
    pending: (oneshot::Sender<()>, tokio::task::JoinHandle<Response>),
    status: StatusCode,
) {
    pending.0.send(()).unwrap();
    assert_response(pending.1.await.unwrap(), status).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn diagnostics_rejects_revocations_during_collection_without_blocking_mutations() {
    // HTTP mutations also advance the process-wide media epoch. Run this fixture
    // in its own process so parallel media unit tests keep their synthetic grants.
    const ISOLATED: &str = "THISCORD_ADMIN_TEST_ISOLATED";
    if std::env::var_os(ISOLATED).is_none() {
        let status = tokio::task::spawn_blocking(|| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "admin::tests::diagnostics_rejects_revocations_during_collection_without_blocking_mutations", "--include-ignored"])
                .env(ISOLATED, "1")
                .status().unwrap()
        }).await.unwrap();
        assert!(status.success(), "isolated diagnostics regression failed");
        return;
    }
    let db = database();
    let (_, owner) = user(&db, "owner");
    let (admin_id, admin) = user(&db, "admin");
    let (other_id, other) = user(&db, "other");
    crate::permissions::bootstrap_owner(&db.pool, "owner").unwrap();
    let app = crate::api::router(Some(db.pool.clone()), vec![]);
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":true}),
        StatusCode::OK,
    )
    .await;
    let pending = collecting(&db, &admin).await;
    // Rejected writes and unrelated role changes cannot invalidate this snapshot.
    command(
        &app,
        &other,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":false}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":other_id,"admin":true}),
        StatusCode::OK,
    )
    .await;
    finish(pending, StatusCode::OK).await;

    let pending = collecting(&db, &admin).await;
    let second = collecting(&db, &owner).await;
    get(&app, &other, StatusCode::TOO_MANY_REQUESTS).await;
    // Demotion invalidates diagnostics, without revoking this user's chat epoch.
    let chat = access::snapshot(&access::account(admin_id), None).unwrap();
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":false}),
        StatusCode::OK,
    )
    .await;
    assert!(chat.deliver().is_some());
    finish(pending, StatusCode::FORBIDDEN).await;
    finish(second, StatusCode::OK).await;
    get(&app, &admin, StatusCode::FORBIDDEN).await;

    let pending = collecting(&db, &owner).await;
    let successor = collecting(&db, &other).await;
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"transfer_instance","account_id":other_id}),
        StatusCode::OK,
    )
    .await;
    finish(pending, StatusCode::FORBIDDEN).await;
    finish(successor, StatusCode::FORBIDDEN).await;
    get(&app, &owner, StatusCode::FORBIDDEN).await;
    assert_eq!(get(&app, &other, StatusCode::OK).await["role"], "owner");

    let pending = collecting(&db, &other).await;
    command(
        &app,
        &other,
        ACCOUNT_PATH,
        json!({"action":"logout"}),
        StatusCode::OK,
    )
    .await;
    finish(pending, StatusCode::FORBIDDEN).await;
    get(&app, &other, StatusCode::UNAUTHORIZED).await;
}
