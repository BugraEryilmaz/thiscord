use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiscord_backend::{api, db, permissions};
use thiscord_shared::{AccountId, permissions::*};
use tower::ServiceExt;
use uuid::Uuid;
// The running backend deliberately invalidates all sockets on permission changes.
// Keep independent database fixtures from invalidating each other's sockets.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("chat_test_{}", Uuid::new_v4().simple());
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
fn user(db: &Database, name: &str) -> (AccountId, String) {
    let id = AccountId::from_uuid(Uuid::new_v4());
    let token = URL_SAFE_NO_PAD.encode(Sha256::digest(Uuid::new_v4().as_bytes()));
    let sid = Uuid::new_v4();
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)").bind::<Text,_>(id.to_string()).bind::<Text,_>(name).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,'test-unused-hash')").bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO sessions(id,account_id,device,reauthenticated_at) VALUES($1::uuid,$2::uuid,'test',now())").bind::<Text,_>(sid.to_string()).bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query(
        "INSERT INTO session_tokens(token_hash,session_id,active) VALUES($1,$2::uuid,TRUE)",
    )
    .bind::<Text, _>(URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes())))
    .bind::<Text, _>(sid.to_string())
    .execute(&mut c)
    .unwrap();
    (id, token)
}
async fn call(app: &Router, path: &str, token: &str, command: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(command.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap();
    if !status.is_success() {
        assert_eq!(body["request_id"], request_id);
    }
    (status, body)
}
async fn command(app: &Router, token: &str, command: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, PERMISSIONS_PATH, token, command).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn change(
    app: &Router,
    token: &str,
    state: &mut Value,
    change: Value,
    expected: StatusCode,
) -> Value {
    let body=command(app,token,json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"change":change}),expected).await;
    if body["result"] == "state" {
        *state = body["state"].clone();
    }
    body
}

use futures_util::{SinkExt, StreamExt};
use thiscord_shared::chat::*;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
async fn chat(app: &Router, token: &str, request: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, CHAT_PATH, token, request).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn setup(db: &Database) -> (Router, String, String, AccountId, Value, Value) {
    let (_, owner) = user(db, "owner");
    let (guest_id, guest) = user(db, "guest");
    permissions::bootstrap_owner(&db.pool, "owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"chat"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":"guest"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"one","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    (app, owner, guest, guest_id, state, channel)
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn messages_deduplicate_paginate_moderate_and_isolate() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let client = Uuid::new_v4();
    let request = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":client,"content":"Hello @guest <script>alert(1)</script>"});
    let (first, second) = tokio::join!(
        chat(&app, &owner, request.clone(), StatusCode::OK),
        chat(&app, &owner, request.clone(), StatusCode::OK)
    );
    assert_eq!(first["message"]["id"], second["message"]["id"]);
    assert_eq!(first["message"]["mentions"], json!([guest_id]));
    let id = first["message"]["id"].clone();
    let unread = chat(
        &app,
        &guest,
        json!({"action":"unread","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(unread["channels"][0]["count"], 1);
    assert_eq!(unread["channels"][0]["mentions"], 1);
    let edited=chat(&app,&owner,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":id,"revision":0,"content":"edited"}),StatusCode::OK).await;
    assert_eq!(edited["message"]["revision"], 1);
    let retry = chat(&app, &owner, request.clone(), StatusCode::OK).await;
    assert_eq!(retry["message"]["content"], "edited");
    chat(&app,&guest,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":id,"revision":1,"content":"takeover"}),StatusCode::FORBIDDEN).await;
    chat(&app,&owner,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":id,"revision":0,"content":"stale"}),StatusCode::CONFLICT).await;
    for content in ["second", "third"] {
        chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":content}),StatusCode::OK).await;
    }
    let page = chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":channel,"limit":1}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["history"]["messages"][0]["content"], "third");
    let older=chat(&app,&guest,json!({"action":"history","guild_id":guild,"channel_id":channel,"limit":1,"before":page["history"]["older"]}),StatusCode::OK).await;
    assert_eq!(older["history"]["messages"][0]["content"], "second");
    chat(&app,&guest,json!({"action":"read","guild_id":guild,"channel_id":channel,"through":page["history"]["messages"][0]["sequence"]}),StatusCode::OK).await;
    assert_eq!(
        chat(
            &app,
            &guest,
            json!({"action":"unread","guild_id":guild}),
            StatusCode::OK
        )
        .await["channels"],
        json!([])
    );
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"two","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let other = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    chat(
        &app,
        &owner,
        json!({"action":"delete","guild_id":guild,"channel_id":other,"message_id":id,"revision":1}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let deleted=chat(&app,&owner,json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":id,"revision":1}),StatusCode::OK).await;
    assert_eq!(deleted["message"]["content"], "");
    assert_eq!(deleted["message"]["deleted"], true);
    assert_eq!(
        chat(&app, &owner, request, StatusCode::OK).await["message"]["deleted"],
        true
    );
    let (_, outsider) = user(&db, "outsider");
    chat(
        &app,
        &outsider,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::FORBIDDEN,
    )
    .await;
    chat(&app,&guest,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"x".repeat(4001)}),StatusCode::BAD_REQUEST).await;
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::FORBIDDEN,
    )
    .await;
}
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn open_socket(
    address: std::net::SocketAddr,
    token: &str,
    guild: &Value,
    channel: &Value,
) -> Socket {
    let mut request = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket.send(WsMessage::Text(json!({"version":1,"event":{"type":"authenticate","token":token,"guild_id":guild,"channel_id":channel}}).to_string().into())).await.unwrap();
    socket
}
async fn event(socket: &mut Socket, kind: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            let incoming = socket.next().await.unwrap().unwrap();
            if let WsMessage::Text(text) = incoming {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["version"], 1);
                if value["event"]["type"] == kind {
                    return value["event"].clone();
                }
            }
        }
    })
    .await
    .expect("socket event timeout")
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn sockets_authenticate_reconnect_and_revoke() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"isolated","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let isolated = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut forbidden = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    forbidden
        .headers_mut()
        .insert("origin", "https://untrusted.example".parse().unwrap());
    assert!(connect_async(forbidden).await.is_err());
    let mut bad = open_socket(address, "bad", &guild, &channel).await;
    event(&mut bad, "error").await;
    drop(bad);
    let mut socket = open_socket(address, &guest, &guild, &channel).await;
    event(&mut socket, "ready").await;
    // Rejected mutations and random credentials must not disconnect valid clients.
    command(
        &app,
        "invalid",
        json!({"action":"change","guild_id":guild,"revision":0,"change":{"action":"delete"}}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let mut invalid = open_socket(address, "invalid", &guild, &channel).await;
    event(&mut invalid, "error").await;
    drop(invalid);
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"ping"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    event(&mut socket, "pong").await;
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"typing","active":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let presence = event(&mut socket, "presence").await;
    assert_eq!(presence["members"][0]["account_id"], json!(guest_id));
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":isolated,"client_id":Uuid::new_v4(),"content":"not for this subscription"}),StatusCode::OK).await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"live"}),StatusCode::OK).await;
    assert_eq!(
        event(&mut socket, "message").await["message"]["content"],
        "live"
    );
    socket.close(None).await.unwrap();
    drop(socket);
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"while disconnected"}),StatusCode::OK).await;
    let mut socket = open_socket(address, &guest, &guild, &channel).await;
    assert_eq!(
        event(&mut socket, "ready").await["history"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    event(&mut socket, "revoked").await;
    drop(socket);
    let mut denied = open_socket(address, &guest, &guild, &channel).await;
    event(&mut denied, "error").await;
    drop(denied);
    let mut socket = open_socket(address, &owner, &guild, &channel).await;
    event(&mut socket, "ready").await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"delete_channel","channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    event(&mut socket, "revoked").await;
    drop(socket);
    let mut socket = open_socket(address, &owner, &guild, &isolated).await;
    event(&mut socket, "ready").await;
    for _ in 0..31 {
        let _ = socket
            .send(WsMessage::Text(
                json!({"version":1,"event":{"type":"ping"}})
                    .to_string()
                    .into(),
            ))
            .await;
    }
    assert_eq!(
        event(&mut socket, "error").await["error"]["code"],
        "rate_limited"
    );
    drop(socket);
    let mut socket = open_socket(address, &owner, &guild, &isolated).await;
    event(&mut socket, "ready").await;
    let (status, _) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &owner,
        json!({"action":"logout"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    event(&mut socket, "revoked").await;
    drop(socket);
    server.abort();
}
