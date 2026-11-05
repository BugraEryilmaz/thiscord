//! Admitted sockets ride out database unavailability: nothing that needs the
//! database is delivered meanwhile, no `revoked` or error frame is sent, and
//! delivery resumes once the database is back.
use super::*;
use std::time::Duration;

/// A one-connection pool on the test schema. A router built on it loses its
/// database while the test holds that connection (checkout times out after two
/// seconds), while HTTP writes through the main pool keep working.
fn small_pool(db: &Database) -> db::DbPool {
    let value = std::env::var("TEST_DATABASE_URL").unwrap();
    let mut url = url::Url::parse(&value).unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", db.schema));
    db::connect_sized(url.as_str(), 1).unwrap()
}

async fn serve(app: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        address,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}

/// Frames for `duration`; fails on any revocation, error or closed socket.
async fn frames_while_unavailable(socket: &mut Socket, duration: Duration) -> Vec<Value> {
    let mut frames = Vec::new();
    let _ = tokio::time::timeout(duration, async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    let kind = &frame["event"]["type"];
                    assert!(kind != "revoked" && kind != "error", "{frame}");
                    assert_ne!(frame["event"]["event"]["type"], "error", "{frame}");
                    frames.push(frame);
                }
                Some(Ok(_)) => {}
                other => panic!("socket closed during database trouble: {other:?}"),
            }
        }
    })
    .await;
    frames
}

/// The first text frame matching `predicate` within ten seconds (the backoff
/// after a five-second outage can wait up to six seconds).
async fn recovered(socket: &mut Socket, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Ok(WsMessage::Text(text))) = socket.next().await {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if predicate(&frame) {
                    return frame;
                }
            }
        }
    })
    .await
    .expect("delivery must resume once the database is back")
}

const OUTAGE: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn chat_socket_survives_database_unavailability_and_recovers() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let pool = small_pool(&db);
    let (address, server) = serve(api::router(
        Some(pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    ))
    .await;
    let mut socket = session_socket(address, &guest).await;
    event(&mut socket, "authenticated").await;
    subscribe(&mut socket, 1, guild.clone(), channel.clone()).await;
    let message = |content: &str| json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":content});
    chat(&app, &owner, message("before"), StatusCode::OK).await;
    assert_eq!(
        pushed(&mut socket, 1, "message").await["message"]["content"],
        "before"
    );
    let held = pool.get().unwrap();
    chat(&app, &owner, message("during"), StatusCode::OK).await;
    let frames = frames_while_unavailable(&mut socket, OUTAGE).await;
    assert!(
        !frames
            .iter()
            .any(|f| f["event"]["event"]["type"] == "message"),
        "a message needing the database was delivered without it"
    );
    drop(held);
    let resumed = recovered(&mut socket, |f| f["event"]["event"]["type"] == "message").await;
    assert_eq!(resumed["event"]["event"]["message"]["content"], "during");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn notification_socket_survives_database_unavailability_and_recovers() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, first) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
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
        .find(|c| c["id"] != first)
        .unwrap()["id"]
        .clone();
    let pool = small_pool(&db);
    let (address, server) = serve(api::router(
        Some(pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    ))
    .await;
    let mut socket = {
        let mut request = format!(
            "ws://{address}{}",
            thiscord_shared::notifications::SOCKET_PATH
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("origin", "http://localhost:1420".parse().unwrap());
        connect_async(request).await.unwrap().0
    };
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"connect","token":guest,"kinds":["guild_message"]}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    recovered(&mut socket, |f| f["event"]["type"] == "calls").await;
    let message = |channel: &Value, content: &str| json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":content});
    let alert = |frame: &Value| frame["event"]["type"] == "alert";
    chat(&app, &owner, message(&first, "first"), StatusCode::OK).await;
    recovered(&mut socket, alert).await;
    // An alert in a channel without a reusable grant needs the database.
    let held = pool.get().unwrap();
    chat(&app, &owner, message(&second, "during"), StatusCode::OK).await;
    let frames = frames_while_unavailable(&mut socket, OUTAGE).await;
    assert!(
        !frames.iter().any(alert),
        "an alert was authorized without the database"
    );
    drop(held);
    // The skipped alert is not replayed; later ones arrive on the same socket.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let sent = chat(&app, &owner, message(&second, "after"), StatusCode::OK).await;
    let resumed = recovered(&mut socket, alert).await;
    assert_eq!(resumed["event"]["alert"]["id"], sent["message"]["id"]);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn friends_socket_survives_database_unavailability_and_recovers() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (_, alice) = user(&db, "outage_alice");
    let (bob_id, bob) = user(&db, "outage_bob");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let pool = small_pool(&db);
    let (address, server) = serve(api::router(
        Some(pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    ))
    .await;
    let mut socket = friends::socket(address, &bob).await;
    friends::snapshot(&mut socket, |_| true).await;
    let held = pool.get().unwrap();
    // The request commits through the main pool and wakes Bob's socket, whose
    // snapshot read cannot reach the database.
    friends::social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    let frames = frames_while_unavailable(&mut socket, OUTAGE).await;
    assert!(
        !frames
            .iter()
            .any(|f| f["event"]["snapshot"]["friends"][0]["relationship"] == "incoming"),
        "a snapshot needing the database was delivered without it"
    );
    drop(held);
    recovered(&mut socket, |f| {
        f["event"]["type"] == "snapshot"
            && f["event"]["snapshot"]["friends"][0]["relationship"] == "incoming"
    })
    .await;
    server.abort();
}
