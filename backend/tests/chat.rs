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
    diesel::sql_query("INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2||' Profile',TRUE)").bind::<Text,_>(id.to_string()).bind::<Text,_>(name).execute(&mut c).unwrap();
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
    assert_eq!(first["message"]["username"], "owner");
    assert_eq!(first["message"]["display_name"], "owner Profile");
    assert!(
        state["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["display_name"].as_str().unwrap().ends_with(" Profile"))
    );
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
    assert_eq!(
        page["history"]["messages"][0]["display_name"],
        "owner Profile"
    );
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
    assert_eq!(presence["members"][0]["display_name"], "guest Profile");
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

async fn session_socket(address: std::net::SocketAddr, token: &str) -> Socket {
    let mut request = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"connect","token":token}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
}
async fn subscribe(socket: &mut Socket, serial: u64, guild: Value, channel: Value) -> Value {
    socket.send(WsMessage::Text(json!({"version":1,"event":{"type":"subscribe","subscription":serial,"guild_id":guild,"channel_id":channel}}).to_string().into())).await.unwrap();
    let ready = event(socket, "subscribed").await;
    assert_eq!(ready["subscription"], serial);
    ready
}
async fn pushed(socket: &mut Socket, serial: u64, kind: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let update = event(socket, "update").await;
            if update["subscription"] == serial && update["event"]["type"] == kind {
                return update["event"].clone();
            }
        }
    })
    .await
    .expect("commit did not push an update")
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn session_socket_subscriptions_push_unread_switch_resync_and_revoke() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"second","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let second = state["channels"]
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
    let mut invalid = session_socket(address, "invalid").await;
    event(&mut invalid, "error").await;
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    // Authentication is independent of selecting a guild or channel.
    assert!(subscribe(&mut socket, 1, Value::Null, Value::Null).await["history"].is_null());
    subscribe(&mut socket, 2, guild.clone(), Value::Null).await;
    assert_eq!(
        pushed(&mut socket, 2, "unread").await["channels"],
        json!([])
    );
    let sent = chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"push unread"}), StatusCode::OK).await;
    assert_eq!(
        pushed(&mut socket, 2, "unread").await["channels"][0]["count"],
        1
    );
    let ready = subscribe(&mut socket, 3, guild.clone(), channel.clone()).await;
    assert_eq!(ready["history"]["messages"][0]["content"], "push unread");
    pushed(&mut socket, 3, "unread").await;
    chat(&app, &guest, json!({"action":"read","guild_id":guild,"channel_id":channel,"through":sent["message"]["sequence"]}), StatusCode::OK).await;
    assert_eq!(
        pushed(&mut socket, 3, "unread").await["channels"],
        json!([])
    );
    // Reuse the same transport for another channel; the old channel's body
    // must never arrive under the new subscription ID.
    subscribe(&mut socket, 4, guild.clone(), second.clone()).await;
    pushed(&mut socket, 4, "unread").await;
    chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"old channel"}), StatusCode::OK).await;
    chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":second,"client_id":Uuid::new_v4(),"content":"new channel"}), StatusCode::OK).await;
    assert_eq!(
        pushed(&mut socket, 4, "message").await["message"]["content"],
        "new channel"
    );
    socket.close(None).await.unwrap();
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    assert_eq!(
        subscribe(&mut socket, 1, guild.clone(), second.clone()).await["history"]["messages"][0]["content"],
        "new channel"
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
    let mut denied = session_socket(address, &guest).await;
    event(&mut denied, "authenticated").await;
    denied.send(WsMessage::Text(json!({"version":1,"event":{"type":"subscribe","subscription":1,"guild_id":guild,"channel_id":second}}).to_string().into())).await.unwrap();
    assert_eq!(
        pushed(&mut denied, 1, "error").await["error"]["code"],
        "forbidden"
    );
    drop(denied);
    drop(socket);
    drop(invalid);
    server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn timeout_revokes_chat_and_expiry_refreshes_composer_permissions() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    subscribe(&mut socket, 1, guild.clone(), channel.clone()).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"timeout_member","account_id":guest_id,"duration_seconds":60}),
        StatusCode::OK,
    )
    .await;
    event(&mut socket, "revoked").await;
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    let ready = subscribe(&mut socket, 1, guild.clone(), channel.clone()).await;
    assert!(
        !ready["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("send_messages"))
    );
    assert!(
        ready["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("read_history"))
    );
    diesel::sql_query("UPDATE guild_moderation SET timeout_until=now()-interval '1 second' WHERE guild_id=$1::uuid AND account_id=$2::uuid").bind::<Text,_>(guild.as_str().unwrap()).bind::<Text,_>(guest_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    // No mutation notification: the normal maintenance tick must detect expiry.
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        loop {
            let incoming = socket.next().await.unwrap().unwrap();
            if let WsMessage::Text(text) = incoming {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["event"]["type"] == "revoked" {
                    break;
                }
            }
        }
    })
    .await
    .expect("timeout expiry did not refresh the socket");
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    let ready = subscribe(&mut socket, 1, guild, channel).await;
    assert!(
        ready["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("send_messages"))
    );
    socket.close(None).await.unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn authorization_readers_share_locks_and_activity_is_coalesced() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, _, guest, guest_id, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let history = json!({"action":"history","guild_id":guild,"channel_id":channel});
    let mut reader = db.pool.get().unwrap();
    reader.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM accounts WHERE id=$1::uuid FOR SHARE")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut reader)
        .unwrap();
    diesel::sql_query("SELECT id FROM sessions WHERE account_id=$1::uuid FOR SHARE")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut reader)
        .unwrap();
    diesel::sql_query("SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE")
        .bind::<Text, _>(guild.as_str().unwrap())
        .execute(&mut reader)
        .unwrap();
    diesel::sql_query("SELECT id FROM channels WHERE id=$1::uuid FOR SHARE")
        .bind::<Text, _>(channel.as_str().unwrap())
        .execute(&mut reader)
        .unwrap();
    // These requests fail with lock_timeout if any routine check still takes
    // FOR UPDATE or writes last_seen_at. Same-session readers must coexist.
    let (a, b) = tokio::join!(
        chat(&app, &guest, history.clone(), StatusCode::OK),
        chat(&app, &guest, history.clone(), StatusCode::OK),
    );
    assert_eq!(a, b);
    command(
        &app,
        &guest,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    reader.batch_execute("ROLLBACK").unwrap();

    #[derive(QueryableByName)]
    struct Activity {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        fresh: bool,
        #[diesel(sql_type = Text)]
        seen: String,
    }
    diesel::sql_query(
        "UPDATE sessions SET last_seen_at=now()-interval '6 minutes' WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(guest_id.to_string())
    .execute(&mut reader)
    .unwrap();
    chat(&app, &guest, history.clone(), StatusCode::OK).await;
    let activity = |c: &mut PgConnection| {
        diesel::sql_query("SELECT last_seen_at>now()-interval '1 minute' AS fresh,last_seen_at::text AS seen FROM sessions WHERE account_id=$1::uuid").bind::<Text,_>(guest_id.to_string()).get_result::<Activity>(c).unwrap()
    };
    let first = activity(&mut reader);
    assert!(first.fresh);
    chat(&app, &guest, history.clone(), StatusCode::OK).await;
    assert_eq!(first.seen, activity(&mut reader).seen);
    diesel::sql_query(
        "UPDATE sessions SET last_seen_at=now()-interval '8 days' WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(guest_id.to_string())
    .execute(&mut reader)
    .unwrap();
    chat(&app, &guest, history.clone(), StatusCode::UNAUTHORIZED).await;

    diesel::sql_query("UPDATE sessions SET last_seen_at=now() WHERE account_id=$1::uuid")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut reader)
        .unwrap();
    reader.batch_execute("BEGIN").unwrap();
    diesel::sql_query("UPDATE sessions SET revoked=TRUE WHERE account_id=$1::uuid")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut reader)
        .unwrap();
    // The optimistic authentication lookup can still see the old committed row;
    // the action's shared session lock must wait and reject the committed revoke.
    let request = call(&app, CHAT_PATH, &guest, history);
    tokio::pin!(request);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    reader.batch_execute("COMMIT").unwrap();
    assert_eq!(request.await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn channel_writes_serialize_without_blocking_other_channels() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, first) = setup(&db).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"two","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let second = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ch| ch["id"] != first)
        .unwrap()["id"]
        .clone();
    let guild = state["guild"]["id"].clone();
    let mut writer = db.pool.get().unwrap();
    writer.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE")
        .bind::<Text, _>(guild.as_str().unwrap())
        .execute(&mut writer)
        .unwrap();
    diesel::sql_query("SELECT id FROM channels WHERE id=$1::uuid FOR UPDATE")
        .bind::<Text, _>(first.as_str().unwrap())
        .execute(&mut writer)
        .unwrap();
    let sent = chat(&app, &guest, json!({"action":"send","guild_id":guild,"channel_id":second,"client_id":Uuid::new_v4(),"content":"independent channel"}), StatusCode::OK).await;
    chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":second}),
        StatusCode::OK,
    )
    .await;
    writer.batch_execute("ROLLBACK").unwrap();
    let id = sent["message"]["id"].clone();
    let edit = json!({"action":"edit","guild_id":guild,"channel_id":second,"message_id":id,"revision":sent["message"]["revision"],"content":"concurrent edit"});
    let (a, b) = tokio::join!(
        call(&app, CHAT_PATH, &guest, edit.clone()),
        call(&app, CHAT_PATH, &guest, edit)
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT)
    );
    let client = Uuid::new_v4();
    let send = |channel: &Value| json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":client,"content":"same key in different channels"});
    let (a, b) = tokio::join!(
        call(&app, CHAT_PATH, &guest, send(&first)),
        call(&app, CHAT_PATH, &guest, send(&second))
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT)
    );

    // A permission writer excludes readers until commit, after which the waiting
    // reader must evaluate the new grants, not a snapshot taken before the lock.
    writer.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE")
        .bind::<Text, _>(guild.as_str().unwrap())
        .execute(&mut writer)
        .unwrap();
    diesel::sql_query(
        "UPDATE guild_roles SET permissions='[]' WHERE guild_id=$1::uuid AND everyone",
    )
    .bind::<Text, _>(guild.as_str().unwrap())
    .execute(&mut writer)
    .unwrap();
    let request = call(
        &app,
        CHAT_PATH,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":second}),
    );
    tokio::pin!(request);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    writer.batch_execute("COMMIT").unwrap();
    assert_eq!(request.await.0, StatusCode::FORBIDDEN);
}
