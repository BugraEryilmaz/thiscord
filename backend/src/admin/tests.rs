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

    // Logout revokes this session while another device of the same account
    // retains its already-authorized diagnostics response.
    let second_token = store::digest(&Uuid::new_v4().to_string());
    {
        let mut c = db.pool.get().unwrap();
        let sid = Uuid::new_v4().to_string();
        store::execute(
            &mut c,
            "INSERT INTO sessions(id,account_id,device) VALUES($1::uuid,$2::uuid,'second')",
            &[&sid, &other_id.to_string()],
        )
        .unwrap();
        store::execute(
            &mut c,
            "INSERT INTO session_tokens(token_hash,session_id) VALUES($1,$2::uuid)",
            &[&store::digest(&second_token), &sid],
        )
        .unwrap();
    }
    let second = collecting(&db, &second_token).await;
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
    finish(second, StatusCode::OK).await;
    get(&app, &other, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn remote_audio_rechecks_instance_privileges_and_never_retargets_connections() {
    let db = database();
    let (_, owner) = user(&db, "remote_owner");
    let (admin_id, admin) = user(&db, "remote_admin");
    let (_, ordinary) = user(&db, "remote_user");
    crate::permissions::bootstrap_owner(&db.pool, "remote_owner").unwrap();
    let app = crate::api::router(Some(db.pool.clone()), vec![]);
    let connection = RequestId::from_uuid(Uuid::new_v4());
    let (registration, mut requests) = super::audio::register(connection);
    let path = thiscord_shared::remote_audio::PATH;
    let request = json!({"connection_id":connection,"command":{"command":"audio_current"}});
    command(
        &app,
        &ordinary,
        path,
        request.clone(),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(&app, "", path, request.clone(), StatusCode::UNAUTHORIZED).await;
    assert!(requests.try_recv().is_err());
    command(
        &app,
        &owner,
        path,
        json!({"connection_id":connection,"command":{"command":"audio_debug_start"}}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":true}),
        StatusCode::OK,
    )
    .await;
    let post = |token: String| {
        let app = app.clone();
        let request = request.clone();
        tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(thiscord_shared::remote_audio::PATH)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        })
    };
    let response = post(admin.clone());
    let mut pending = tokio::time::timeout(std::time::Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(pending.authorized.clone().deliver().is_some());
    pending.chunk(r#"{"Ok":{"output_volume":0.5}}"#).unwrap();
    pending.finish();
    let value = assert_response(response.await.unwrap(), StatusCode::OK).await;
    assert_eq!(value["Ok"]["output_volume"], 0.5);

    let response = post(admin.clone());
    let mut pending = requests.recv().await.unwrap();
    command(
        &app,
        &owner,
        PERMISSIONS_PATH,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":false}),
        StatusCode::OK,
    )
    .await;
    assert!(
        pending.authorized.clone().deliver().is_none(),
        "demotion rejects queued dispatch"
    );
    pending
        .chunk(r#"{"Ok":{"private":"diagnostics"}}"#)
        .unwrap();
    pending.finish();
    assert_response(response.await.unwrap(), StatusCode::FORBIDDEN).await;
    command(&app, &admin, path, request.clone(), StatusCode::FORBIDDEN).await;

    let response = post(owner.clone());
    let pending = requests.recv().await.unwrap();
    drop(registration);
    drop(pending);
    drop(requests);
    assert_response(response.await.unwrap(), StatusCode::CONFLICT).await;
    assert!(!super::audio::available(connection));
    let (_replacement, _replacement_requests) =
        super::audio::register(RequestId::from_uuid(Uuid::new_v4()));
    command(&app, &owner, path, request, StatusCode::CONFLICT).await;
}
