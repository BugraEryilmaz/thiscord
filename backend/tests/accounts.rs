use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use serde_json::{Value, json};
use std::{
    env,
    net::SocketAddr,
    sync::atomic::{AtomicU16, Ordering},
};
use thiscord_backend::{api, db};
use thiscord_shared::{ApiError, account::*};
use tower::ServiceExt;
use uuid::Uuid;

// Independent fixtures share the process-wide four-worker account limit. Each
// fixture sends requests sequentially; overload behavior is tested in chat.rs.
static FIXTURES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

struct Database {
    connection: PgConnection,
    schema: String,
    pool: db::DbPool,
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
    let value = env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("accounts_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = db::connect_and_migrate(url.as_str()).unwrap();
    Database {
        connection,
        schema,
        pool,
    }
}
async fn call(app: &Router, command: Value, token: Option<&str>, expected: StatusCode) -> Value {
    static NEXT: AtomicU16 = AtomicU16::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut builder = Request::builder()
        .method("POST")
        .uri(ACCOUNT_PATH)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let mut request = builder.body(Body::from(command.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((
            [10, 0, (n / 255) as u8, (n % 255) as u8],
            12345,
        ))));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        status,
        expected,
        "unexpected account result: {}",
        body.get("message").unwrap_or(&Value::Null)
    );
    if !status.is_success() {
        let error: ApiError = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(error.request_id.to_string(), request_id);
    }
    body
}
#[derive(QueryableByName)]
struct BodyRow {
    #[diesel(sql_type=Text)]
    body: String,
}
fn mail_value(db: &Database, purpose: &str) -> String {
    let body = diesel::sql_query(
        "SELECT body FROM mail_outbox WHERE body LIKE $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind::<Text, _>(format!("Thiscord {purpose}%"))
    .get_result::<BodyRow>(&mut db.pool.get().unwrap())
    .unwrap()
    .body;
    body.split("\n\n").nth(1).unwrap().to_string()
}
fn code(db: &Database, purpose: &str) -> String {
    let value = mail_value(db, purpose);
    if matches!(purpose, "verify" | "reset") {
        url::Url::parse(&value)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "token")
            .unwrap()
            .1
            .into_owned()
    } else {
        value
    }
}

async fn reset_page(
    app: &Router,
    method: &str,
    uri: &str,
    fields: &[(&str, &str)],
    expected: StatusCode,
) -> String {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self'")
    );
    assert!(response.headers().contains_key("x-request-id"));
    if expected == StatusCode::TOO_MANY_REQUESTS {
        assert_eq!(response.headers()["retry-after"], "60");
    }
    String::from_utf8(
        to_bytes(response.into_body(), 16384)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn reset_browser_rejects_malformed_inputs_and_handles_unavailable_database() {
    let app = api::router(None, vec![]);
    for query in ["", "?token=short", "?token=%22%3E%3Cscript%3E"] {
        let html = reset_page(
            &app,
            "GET",
            &format!("{PASSWORD_RESET_PATH}{query}"),
            &[],
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert!(html.contains("This reset link is no longer valid"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<form"));
    }
    let token = "a".repeat(43);
    let json = reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token=a&token=b"),
        &[],
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(serde_json::from_str::<ApiError>(&json).is_ok());
    let html = reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token={token}"),
        &[],
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert!(!html.contains(&token));
    let json = reset_page(
        &app,
        "POST",
        PASSWORD_RESET_PATH,
        &[("token", &token)],
        StatusCode::BAD_REQUEST,
    )
    .await;
    let error: ApiError = serde_json::from_str(&json).unwrap();
    assert_eq!(error.code, thiscord_shared::ErrorCode::ValidationFailed);
    let oversized = "x".repeat(17 * 1024);
    let json = reset_page(
        &app,
        "POST",
        PASSWORD_RESET_PATH,
        &[
            ("token", &token),
            ("password", &oversized),
            ("confirm_password", &oversized),
        ],
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(serde_json::from_str::<ApiError>(&json).is_ok());
    assert!(
        reset_page(
            &app,
            "HEAD",
            PASSWORD_RESET_PATH,
            &[],
            StatusCode::NO_CONTENT
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn reset_browser_links_and_backup_codes_share_single_use_lifecycle() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let session = token(&call(&app, json!({"action":"register","username":"browser_reset","email":"reset@example.test","password":"original password","device":"test"}), None, StatusCode::OK).await);
    let verify = code(&db, "verify");
    // A verification token cannot be used as a reset token.
    reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token={verify}"),
        &[],
        StatusCode::BAD_REQUEST,
    )
    .await;
    call(
        &app,
        json!({"action":"verify_email","code":verify}),
        None,
        StatusCode::OK,
    )
    .await;
    let request = json!({"action":"forgot_password","email":"reset@example.test"});
    call(&app, request.clone(), None, StatusCode::OK).await;
    let link = url::Url::parse(&mail_value(&db, "reset")).unwrap();
    assert_eq!(link.path(), PASSWORD_RESET_PATH);
    let uri = format!("{}?{}", link.path(), link.query().unwrap());
    let reset = code(&db, "reset");
    let body = diesel::sql_query("SELECT body FROM mail_outbox WHERE body LIKE 'Thiscord reset%' ORDER BY created_at DESC LIMIT 1")
        .get_result::<BodyRow>(&mut db.pool.get().unwrap()).unwrap().body;
    assert!(body.contains(&format!("Backup reset code:\n\n{reset}\n\n")));
    reset_page(&app, "HEAD", &uri, &[], StatusCode::NO_CONTENT).await;
    for _ in 0..2 {
        let html = reset_page(&app, "GET", &uri, &[], StatusCode::OK).await;
        assert!(html.contains("Choose a new password"));
        assert!(html.contains(&format!("name=\"token\" value=\"{reset}\"")));
        assert!(html.contains(&format!("method=\"post\" action=\"{PASSWORD_RESET_PATH}\"")));
    }
    // Opening/scanning the link does not change the password or revoke sessions.
    call(
        &app,
        json!({"action":"current"}),
        Some(&session),
        StatusCode::OK,
    )
    .await;
    let submit = |password: &str, confirmation: &str| {
        vec![
            ("token".to_owned(), reset.clone()),
            ("password".to_owned(), password.to_owned()),
            ("confirm_password".to_owned(), confirmation.to_owned()),
        ]
    };
    for (password, confirmation, message) in [
        (
            "new password with <>&+",
            "different password",
            "Passwords do not match",
        ),
        ("short", "short", "at least 12 characters"),
    ] {
        let values = submit(password, confirmation);
        let fields: Vec<_> = values
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let html = reset_page(
            &app,
            "POST",
            PASSWORD_RESET_PATH,
            &fields,
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert!(html.contains(message));
        assert!(html.contains("<form"));
        assert!(!html.contains(password));
    }
    let password = "new password with <>&+";
    let values = submit(password, password);
    let fields: Vec<_> = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let html = reset_page(&app, "POST", PASSWORD_RESET_PATH, &fields, StatusCode::OK).await;
    assert!(html.contains("Password changed"));
    assert!(!html.contains(&reset));
    assert!(!html.contains(password));
    assert!(!html.contains("<form"));
    reset_page(
        &app,
        "POST",
        PASSWORD_RESET_PATH,
        &fields,
        StatusCode::BAD_REQUEST,
    )
    .await;
    reset_page(&app, "GET", &uri, &[], StatusCode::BAD_REQUEST).await;
    call(
        &app,
        json!({"action":"reset_password","code":reset,"password":"another password"}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&session),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    call(&app, json!({"action":"login","login":"browser_reset","password":"original password","device":"test"}), None, StatusCode::UNAUTHORIZED).await;
    call(
        &app,
        json!({"action":"login","login":"browser_reset","password":password,"device":"test"}),
        None,
        StatusCode::OK,
    )
    .await;
    // A resend invalidates the older link, and using the backup invalidates the new one.
    call(&app, request.clone(), None, StatusCode::OK).await;
    let old = code(&db, "reset");
    call(&app, request.clone(), None, StatusCode::OK).await;
    reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token={old}"),
        &[],
        StatusCode::BAD_REQUEST,
    )
    .await;
    let backup = code(&db, "reset");
    call(
        &app,
        json!({"action":"reset_password","code":backup,"password":"backup password"}),
        None,
        StatusCode::OK,
    )
    .await;
    reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token={backup}"),
        &[],
        StatusCode::BAD_REQUEST,
    )
    .await;
    call(&app, request, None, StatusCode::OK).await;
    let expired = code(&db, "reset");
    sql(
        &db,
        "UPDATE account_codes SET expires_at=now()-interval '1 second' WHERE purpose='reset'",
    );
    reset_page(
        &app,
        "GET",
        &format!("{PASSWORD_RESET_PATH}?token={expired}"),
        &[],
        StatusCode::BAD_REQUEST,
    )
    .await;
    reset_page(
        &app,
        "POST",
        PASSWORD_RESET_PATH,
        &[
            ("token", &expired),
            ("password", password),
            ("confirm_password", password),
        ],
        StatusCode::BAD_REQUEST,
    )
    .await;
    // Browser attempts use the same durable per-IP sensitive-operation budget.
    sql(&db, "DELETE FROM auth_limits");
    for _ in 0..30 {
        reset_page(
            &app,
            "POST",
            PASSWORD_RESET_PATH,
            &[
                ("token", &expired),
                ("password", password),
                ("confirm_password", password),
            ],
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    reset_page(
        &app,
        "POST",
        PASSWORD_RESET_PATH,
        &[
            ("token", &expired),
            ("password", password),
            ("confirm_password", password),
        ],
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn profile_pictures_are_owned_persistent_replaceable_and_deleted_with_account() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    use base64::{Engine, engine::general_purpose::STANDARD};
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let owner = token(&call(&app, json!({"action":"register","username":"picture_owner","email":"picture@example.test","password":"a long test password","device":"test"}), None, StatusCode::OK).await);
    let other = token(&call(&app, json!({"action":"register","username":"picture_other","email":"other@example.test","password":"a long test password","device":"test"}), None, StatusCode::OK).await);
    let mut input = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(40, 80)
        .write_to(&mut input, image::ImageFormat::Jpeg)
        .unwrap();
    let upload = json!({"action":"set_avatar","image_base64":STANDARD.encode(input.into_inner())});
    call(&app, upload.clone(), None, StatusCode::UNAUTHORIZED).await;
    let saved = call(&app, upload.clone(), Some(&owner), StatusCode::OK).await;
    let first = saved["account"]["avatar_id"].as_str().unwrap().to_owned();
    let picture = |id: &str| {
        Request::builder()
            .uri(format!("{AVATAR_PATH}/{id}"))
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(picture(&first)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let png = to_bytes(response.into_body(), 300_000).await.unwrap();
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (AVATAR_SIZE, AVATAR_SIZE)
    );
    // No in-memory service state is required to retrieve the saved picture.
    let restarted = api::router(Some(db.pool.clone()), vec![]);
    assert_eq!(
        restarted.oneshot(picture(&first)).await.unwrap().status(),
        StatusCode::OK
    );
    for invalid in [
        json!({"action":"set_avatar","image_base64":"bad image"}),
        json!({"action":"set_avatar","image_base64":"x".repeat(MAX_AVATAR_BASE64+1)}),
    ] {
        call(&app, invalid, Some(&owner), StatusCode::BAD_REQUEST).await;
    }
    let current = call(
        &app,
        json!({"action":"current"}),
        Some(&owner),
        StatusCode::OK,
    )
    .await;
    assert_eq!(current["account"]["avatar_id"], first);
    assert!(
        call(
            &app,
            json!({"action":"current"}),
            Some(&other),
            StatusCode::OK
        )
        .await["account"]["avatar_id"]
            .is_null()
    );
    call(
        &app,
        json!({"action":"set_avatar","image_base64":null,"account_id":saved["account"]["id"]}),
        Some(&other),
        StatusCode::BAD_REQUEST,
    )
    .await;
    call(
        &app,
        json!({"action":"set_avatar","image_base64":null}),
        Some(&other),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        app.clone().oneshot(picture(&first)).await.unwrap().status(),
        StatusCode::OK
    );
    let replacement = call(&app, upload.clone(), Some(&owner), StatusCode::OK).await;
    let second = replacement["account"]["avatar_id"].as_str().unwrap();
    assert_ne!(first, second);
    let missing = app.clone().oneshot(picture(&first)).await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let request_id = missing.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let error: ApiError =
        serde_json::from_slice(&to_bytes(missing.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(error.request_id.to_string(), request_id);
    let removed = call(
        &app,
        json!({"action":"set_avatar","image_base64":null}),
        Some(&owner),
        StatusCode::OK,
    )
    .await;
    assert!(removed["account"]["avatar_id"].is_null());
    assert_eq!(
        app.clone().oneshot(picture(second)).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(picture("invalid"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Extractor body limits must keep the shared wire error format.
    call(
        &app,
        json!({"action":"set_avatar","image_base64":"x".repeat(MAX_AVATAR_BASE64+2048)}),
        Some(&owner),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let last = call(&app, upload.clone(), Some(&owner), StatusCode::OK).await;
    call(
        &app,
        json!({"action":"reauthenticate","password":"a long test password"}),
        Some(&owner),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"delete_account","confirmation":"picture_owner"}),
        Some(&owner),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        app.clone()
            .oneshot(picture(last["account"]["avatar_id"].as_str().unwrap()))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    call(&app, upload, Some(&owner), StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn verification_links_are_single_use_and_expire() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let session=token(&call(&app,json!({"action":"register","username":"link_user","email":"link@example.com","password":"a long test password","device":"test"}),None,StatusCode::OK).await);
    let link = mail_value(&db, "verify");
    let url = url::Url::parse(&link).unwrap();
    assert_eq!(url.path(), EMAIL_VERIFICATION_PATH);
    let uri = format!("{}?{}", url.path(), url.query().unwrap());
    let open = |method: &str| {
        Request::builder()
            .method(method)
            .uri(&uri)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(open("HEAD")).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let current = call(
        &app,
        json!({"action":"current"}),
        Some(&session),
        StatusCode::OK,
    )
    .await;
    assert_eq!(current["account"]["email_verified"], false);
    let response = app.clone().oneshot(open("GET")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let html =
        String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap();
    assert!(html.contains("Email verified"));
    assert!(!html.contains(&code(&db, "verify")));
    assert_eq!(
        app.clone().oneshot(open("GET")).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let current = call(
        &app,
        json!({"action":"current"}),
        Some(&session),
        StatusCode::OK,
    )
    .await;
    assert_eq!(current["account"]["email_verified"], true);
    sql(&db, "UPDATE accounts SET email_verified=FALSE");
    call(
        &app,
        json!({"action":"send_verification"}),
        Some(&session),
        StatusCode::OK,
    )
    .await;
    let url = url::Url::parse(&mail_value(&db, "verify")).unwrap();
    sql(
        &db,
        "UPDATE account_codes SET expires_at=now()-interval '1 second'",
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("{}?{}", url.path(), url.query().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
fn sql(db: &Database, command: &str) {
    db.pool.get().unwrap().batch_execute(command).unwrap();
}
fn token(body: &Value) -> String {
    body["session"]["token"].as_str().unwrap().to_owned()
}
fn login(password: &str) -> Value {
    json!({"action":"login","login":"alice","password":password,"device":"test"})
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn accounts_sessions_recovery_and_deletion() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let register = json!({"action":"register","username":"Alice","email":"Alice@example.com","password":"correct horse battery","device":"laptop"});
    let initial = call(&app, register.clone(), None, StatusCode::OK).await;
    assert_eq!(initial["session"]["account"]["username"], "alice");
    assert_eq!(initial["session"]["account"]["email"], "alice@example.com");
    assert!(!initial.to_string().contains("password_hash"));
    let first = token(&initial);
    call(&app, register, None, StatusCode::CONFLICT).await;
    call(
        &app,
        login("wrong password"),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let verify = code(&db, "verify");
    call(
        &app,
        json!({"action":"verify_email","code":verify}),
        None,
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"verify_email","code":verify}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let current = call(
        &app,
        json!({"action":"current"}),
        Some(&first),
        StatusCode::OK,
    )
    .await;
    assert_eq!(current["account"]["email_verified"], true);
    call(
        &app,
        json!({"action":"update_profile","display_name":"Alice A","bio":"Hello <script>"}),
        Some(&first),
        StatusCode::OK,
    )
    .await;
    let second = token(&call(&app, login("correct horse battery"), None, StatusCode::OK).await);
    let sessions = call(
        &app,
        json!({"action":"sessions"}),
        Some(&first),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 2);
    let other = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == false)
        .unwrap()["id"]
        .clone();
    call(
        &app,
        json!({"action":"revoke_session","session_id":other}),
        Some(&first),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&second),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let rotated = token(
        &call(
            &app,
            json!({"action":"rotate"}),
            Some(&first),
            StatusCode::OK,
        )
        .await,
    );
    call(
        &app,
        json!({"action":"current"}),
        Some(&rotated),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&first),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    // Reuse of a consumed token revokes the entire device session.
    call(
        &app,
        json!({"action":"current"}),
        Some(&rotated),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let active = token(&call(&app, login("correct horse battery"), None, StatusCode::OK).await);
    call(
        &app,
        json!({"action":"change_password","password":"new correct password"}),
        Some(&active),
        StatusCode::FORBIDDEN,
    )
    .await;
    call(
        &app,
        json!({"action":"reauthenticate","password":"correct horse battery"}),
        Some(&active),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"unlink_identity","provider":"password"}),
        Some(&active),
        StatusCode::FORBIDDEN,
    )
    .await;
    call(
        &app,
        json!({"action":"change_password","password":"new correct password"}),
        Some(&active),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&active),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    sql(&db, "DELETE FROM auth_limits");
    call(
        &app,
        login("correct horse battery"),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let active = token(&call(&app, login("new correct password"), None, StatusCode::OK).await);
    let missing = call(
        &app,
        json!({"action":"forgot_password","email":"absent@example.com"}),
        None,
        StatusCode::OK,
    )
    .await;
    let existing = call(
        &app,
        json!({"action":"forgot_password","email":"alice@example.com"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(missing, existing);
    let reset = code(&db, "reset");
    call(
        &app,
        json!({"action":"reset_password","code":reset,"password":"recovered password"}),
        None,
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"reset_password","code":reset,"password":"recovered password"}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&active),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let active = token(&call(&app, login("recovered password"), None, StatusCode::OK).await);
    sql(
        &db,
        "UPDATE sessions SET expires_at=now()-interval '1 second'",
    );
    call(
        &app,
        json!({"action":"current"}),
        Some(&active),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let active = token(&call(&app, login("recovered password"), None, StatusCode::OK).await);
    call(
        &app,
        json!({"action":"logout_all"}),
        Some(&active),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&active),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let active = token(&call(&app, login("recovered password"), None, StatusCode::OK).await);
    call(
        &app,
        json!({"action":"delete_account","confirmation":"alice"}),
        Some(&active),
        StatusCode::FORBIDDEN,
    )
    .await;
    call(
        &app,
        json!({"action":"reauthenticate","password":"recovered password"}),
        Some(&active),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"delete_account","confirmation":"wrong"}),
        Some(&active),
        StatusCode::BAD_REQUEST,
    )
    .await;
    call(
        &app,
        json!({"action":"delete_account","confirmation":"alice"}),
        Some(&active),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"current"}),
        Some(&active),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    sql(&db, "DELETE FROM auth_limits");
    call(
        &app,
        login("recovered password"),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn validation_throttling_and_oauth_rejection() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    call(&app,json!({"action":"register","username":"x","email":"a@example.com","password":"short","device":"test"}),None,StatusCode::BAD_REQUEST).await;
    call(
        &app,
        json!({"action":"unknown_action"}),
        None,
        StatusCode::BAD_REQUEST,
    )
    .await;
    for _ in 0..6 {
        call(
            &app,
            login("invalid password"),
            None,
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    call(
        &app,
        login("invalid password"),
        None,
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    call(
        &app,
        json!({"action":"google_complete","ticket":"unknown"}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    call(&app,json!({"action":"google_start","purpose":"login","device":"test","callback":"https://evil.test"}),None,StatusCode::BAD_REQUEST).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "{GOOGLE_CALLBACK_PATH}?state=unrecognized&code=replayed"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn oauth_cancellation_expiry_and_callback_replay() {
    let _fixture = FIXTURES.acquire().await.unwrap();
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let db = database();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let state = "local-test-state";
    let ticket = URL_SAFE_NO_PAD.encode([7u8; 32]);
    let digest = |s: &str| URL_SAFE_NO_PAD.encode(Sha256::digest(s.as_bytes()));
    let insert = || {
        diesel::sql_query("INSERT INTO oauth_attempts(state_hash,ticket_hash,verifier,nonce,purpose,device) VALUES($1,$2,'test-verifier','test-nonce','login','test')")
        .bind::<Text,_>(digest(state)).bind::<Text,_>(digest(&ticket)).execute(&mut db.pool.get().unwrap()).unwrap()
    };
    insert();
    call(
        &app,
        json!({"action":"google_complete","ticket":ticket}),
        None,
        StatusCode::OK,
    )
    .await;
    let callback = || {
        Request::builder()
            .uri(format!(
                "{GOOGLE_CALLBACK_PATH}?state={state}&error=access_denied"
            ))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(callback()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone().oneshot(callback()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    call(
        &app,
        json!({"action":"google_complete","ticket":ticket}),
        None,
        StatusCode::BAD_REQUEST,
    )
    .await;
    call(
        &app,
        json!({"action":"google_cancel","ticket":ticket}),
        None,
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        json!({"action":"google_complete","ticket":ticket}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    insert();
    sql(
        &db,
        "UPDATE oauth_attempts SET expires_at=now()-interval '1 second'",
    );
    assert_eq!(
        app.clone().oneshot(callback()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    call(
        &app,
        json!({"action":"google_complete","ticket":ticket}),
        None,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}
