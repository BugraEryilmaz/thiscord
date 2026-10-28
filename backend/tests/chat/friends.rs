use super::*;
use thiscord_shared::friends::{FRIENDS_PATH, FRIENDS_SOCKET_PATH};

pub(super) async fn social(app: &Router, token: &str, value: Value, status: StatusCode) -> Value {
    let (got, body) = call(app, FRIENDS_PATH, token, value).await;
    assert_eq!(got, status, "{body}");
    body
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn search_matches_partial_display_names_and_usernames() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (actor_id, actor) = user(&db, "search_actor");
    let (first_id, _) = user(&db, "account_first");
    let (second_id, _) = user(&db, "account_second");
    let (other_id, _) = user(&db, "unrelated");
    let long_name = "界".repeat(64);
    for (id, name) in [
        (actor_id, "Buğra Smith 🎮 100%_"),
        (first_id, "Buğra Smith 🎮 100%_"),
        (second_id, "Buğra Smith 🎮 100%_"),
        (other_id, long_name.as_str()),
    ] {
        diesel::sql_query("UPDATE accounts SET display_name=$2 WHERE id=$1::uuid")
            .bind::<Text, _>(id.to_string())
            .bind::<Text, _>(name)
            .execute(&mut db.pool.get().unwrap())
            .unwrap();
    }
    let app = api::router(Some(db.pool.clone()), vec![]);
    social(
        &app,
        "",
        json!({"action":"search","query":"Smith"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    for query in ["  sMiTh  ", "ĞRA", "Smith 🎮", "100%_", "%_"] {
        let result = social(
            &app,
            &actor,
            json!({"action":"search","query":query}),
            StatusCode::OK,
        )
        .await;
        let people = result["people"].as_array().unwrap();
        assert_eq!(people.len(), 2, "query: {query}");
        assert_eq!(people[0]["id"], json!(first_id));
        assert_eq!(people[1]["id"], json!(second_id));
        assert_eq!(people[0]["username"], "account_first");
        assert_eq!(people[1]["username"], "account_second");
        for person in people {
            assert_eq!(person.as_object().unwrap().len(), 4);
            assert!(person.get("display_name").is_some());
            assert!(person.get("avatar_id").is_some());
        }
    }
    // Username matching also works in the middle, with case ignored.
    let result = social(
        &app,
        &actor,
        json!({"action":"search","query":"UNT_FIR"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(result["people"].as_array().unwrap().len(), 1);
    assert_eq!(result["people"][0]["id"], json!(first_id));
    let result = social(
        &app,
        &actor,
        json!({"action":"search","query":long_name}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(result["people"][0]["id"], json!(other_id));
    for query in ["no match", "__", "%%", "' OR true --"] {
        let result = social(
            &app,
            &actor,
            json!({"action":"search","query":query}),
            StatusCode::OK,
        )
        .await;
        assert_eq!(result["people"], json!([]));
    }
    for query in [
        "".into(),
        "  ".into(),
        "a".into(),
        "界".repeat(65),
        "Sm\nith".into(),
        "Sm\0ith".into(),
    ] {
        social(
            &app,
            &actor,
            json!({"action":"search","query":query}),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    // Broad partial searches stay bounded and have stable ordering.
    for index in 0..22 {
        user(&db, &format!("limit_{index:02}"));
    }
    let result = social(
        &app,
        &actor,
        json!({"action":"search","query":"imit_"}),
        StatusCode::OK,
    )
    .await;
    let people = result["people"].as_array().unwrap();
    assert_eq!(people.len(), 20);
    assert_eq!(people[0]["username"], "limit_00");
    assert_eq!(people[19]["username"], "limit_19");

    // A generated username and an existing pending request must not hide a
    // person when searching part of the display name shown in the friend list.
    let (pending_id, _) = user(&db, "user_5e3b930b842c44b9b4c8");
    diesel::sql_query("UPDATE accounts SET display_name=$2 WHERE id=$1::uuid")
        .bind::<Text, _>(pending_id.to_string())
        .bind::<Text, _>("memedin kocası")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    social(
        &app,
        &actor,
        json!({"action":"add","account_id":pending_id}),
        StatusCode::OK,
    )
    .await;
    let result = social(
        &app,
        &actor,
        json!({"action":"search","query":"memedi"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(result["people"].as_array().unwrap().len(), 1);
    assert_eq!(result["people"][0]["id"], json!(pending_id));
    assert_eq!(result["people"][0]["display_name"], "memedin kocası");
}

pub(super) async fn socket(address: std::net::SocketAddr, token: &str) -> Socket {
    let mut request = format!("ws://{address}{FRIENDS_SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    event(&mut socket, json!({"type":"connect","token":token})).await;
    socket
}
pub(super) async fn event(socket: &mut Socket, event: Value) {
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":event}).to_string().into(),
        ))
        .await
        .unwrap();
}
/// Well under the ten-second database refresh: only commit notifications or
/// scheduled call expiry can deliver these in time.
async fn pushed(socket: &mut Socket, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        snapshot(socket, predicate),
    )
    .await
    .expect("expected a pushed private snapshot")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn commits_and_call_expiry_push_snapshots_before_periodic_refresh() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (alice_id, alice) = user(&db, "push_alice");
    let (bob_id, bob) = user(&db, "push_bob");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let mut a = socket(address, &alice).await;
    let mut b = socket(address, &bob).await;
    snapshot(&mut a, |_| true).await;
    snapshot(&mut b, |_| true).await;

    social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    pushed(&mut b, |s| s["friends"][0]["relationship"] == "incoming").await;
    social(
        &app,
        &bob,
        json!({"action":"accept","account_id":alice_id}),
        StatusCode::OK,
    )
    .await;
    let accepted = pushed(&mut a, |s| s["friends"][0]["relationship"] == "friend").await;
    let conversation = accepted["friends"][0]["conversation_id"].clone();

    let (status, body) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &bob,
        json!({"action":"update_profile","display_name":"Pushed Bob","bio":""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    pushed(&mut a, |s| {
        s["friends"][0]["person"]["display_name"] == "Pushed Bob"
    })
    .await;

    social(&app, &alice, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"Pushed"}), StatusCode::OK).await;
    pushed(&mut b, |s| s["friends"][0]["unread_count"] == 1).await;

    social(
        &app,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await;
    pushed(&mut b, |s| s["calls"][0]["state"] == "ringing").await;
    // Expiry is time-only (no commit); the socket schedules a rebuild for it.
    diesel::sql_query("UPDATE friend_calls SET expires_at=now()+interval '1 second'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let (status, body) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &alice,
        json!({"action":"update_profile","display_name":"Alice again","bio":""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    pushed(&mut b, |s| {
        s["friends"][0]["person"]["display_name"] == "Alice again"
    })
    .await;
    pushed(&mut b, |s| {
        s["calls"].as_array().is_some_and(|c| c.is_empty())
    })
    .await;
    server.abort();
}

pub(super) async fn snapshot(socket: &mut Socket, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let message = socket.next().await.unwrap().unwrap();
            let WsMessage::Text(text) = message else {
                continue;
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            if body["event"]["type"] == "snapshot" && predicate(&body["event"]["snapshot"]) {
                return body["event"]["snapshot"].clone();
            }
        }
    })
    .await
    .expect("expected private snapshot")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn unread_messages_persist_and_reads_are_scoped_monotonic_and_bounded() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (alice_id, alice) = user(&db, "unread_alice");
    let (bob_id, bob) = user(&db, "unread_bob");
    let (_, outsider) = user(&db, "unread_outsider");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    let mut b = socket(address, &bob).await;
    let pending = snapshot(&mut b, |_| true).await;
    let conversation = pending["friends"][0]["conversation_id"].clone();
    assert_eq!(pending["friends"][0]["unread_count"], 0);
    social(
        &app,
        &bob,
        json!({"action":"read","conversation_id":conversation,"through":Uuid::new_v4()}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &bob,
        json!({"action":"accept","account_id":alice_id}),
        StatusCode::OK,
    )
    .await;
    let send = json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"First unread"});
    let first = social(&app, &alice, send.clone(), StatusCode::OK).await["message"]["id"].clone();
    social(&app, &alice, send, StatusCode::OK).await; // Deduplicated sends count once.
    let second = social(&app, &alice, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"Second unread"}), StatusCode::OK).await["message"]["id"].clone();
    snapshot(&mut b, |s| s["friends"][0]["unread_count"] == 2).await;
    let mut a = socket(address, &alice).await;
    assert_eq!(
        snapshot(&mut a, |_| true).await["friends"][0]["unread_count"],
        0
    );
    let mut other_device = socket(address, &bob).await;
    assert_eq!(
        snapshot(&mut other_device, |_| true).await["friends"][0]["unread_count"],
        2
    );

    // History and socket selection do not acknowledge messages on their own.
    social(
        &app,
        &bob,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await;
    event(
        &mut b,
        json!({"type":"select","conversation_id":conversation}),
    )
    .await;
    assert_eq!(
        snapshot(&mut b, |s| s["conversation_id"] == conversation).await["friends"][0]["unread_count"],
        2
    );
    let read_first = json!({"action":"read","conversation_id":conversation,"through":first});
    social(&app, &outsider, read_first.clone(), StatusCode::FORBIDDEN).await;
    social(
        &app,
        &bob,
        json!({"action":"read","conversation_id":conversation,"through":Uuid::new_v4()}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    social(&app, &bob, read_first.clone(), StatusCode::OK).await;
    snapshot(&mut b, |s| s["friends"][0]["unread_count"] == 1).await;
    snapshot(&mut other_device, |s| s["friends"][0]["unread_count"] == 1).await;
    let read_second = json!({"action":"read","conversation_id":conversation,"through":second});
    social(&app, &bob, read_second.clone(), StatusCode::OK).await;
    social(&app, &bob, read_first, StatusCode::OK).await; // Delayed older read cannot regress.
    snapshot(&mut b, |s| s["friends"][0]["unread_count"] == 0).await;
    b.close(None).await.unwrap();
    let mut b = socket(address, &bob).await;
    assert_eq!(
        snapshot(&mut b, |_| true).await["friends"][0]["unread_count"],
        0
    );
    let own = social(&app, &bob, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"Reply"}), StatusCode::OK).await["message"]["id"].clone();
    snapshot(&mut a, |s| s["friends"][0]["unread_count"] == 1).await;
    social(
        &app,
        &alice,
        json!({"action":"read","conversation_id":conversation,"through":own}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut a, |s| s["friends"][0]["unread_count"] == 0).await;

    // Seed a large backlog without exceeding the send-rate budget.
    diesel::sql_query("INSERT INTO direct_messages(id,conversation_id,author_id,client_id,content,created_at) SELECT gen_random_uuid(),$1::uuid,$2::uuid,gen_random_uuid(),'Backlog',clock_timestamp()+n*interval '1 microsecond' FROM generate_series(1,105) n")
        .bind::<Text,_>(conversation.as_str().unwrap()).bind::<Text,_>(alice_id.to_string())
        .execute(&mut db.pool.get().unwrap()).unwrap();
    // Direct SQL bypasses commit notifications; a no-op read wakes Bob's socket.
    social(&app, &bob, read_second.clone(), StatusCode::OK).await;
    snapshot(&mut b, |s| s["friends"][0]["unread_count"] == 100).await;
    social(&app, &bob, read_second.clone(), StatusCode::OK).await;
    let history = social(
        &app,
        &bob,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await;
    let latest = history["history"]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["id"]
        .clone();
    social(
        &app,
        &bob,
        json!({"action":"read","conversation_id":conversation,"through":latest}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut b, |s| s["friends"][0]["unread_count"] == 0).await;

    social(
        &app,
        &alice,
        json!({"action":"remove","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    social(&app, &bob, read_second, StatusCode::FORBIDDEN).await;
    social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    let mut pending_again = socket(address, &bob).await;
    assert_eq!(
        snapshot(&mut pending_again, |_| true).await["friends"][0]["unread_count"],
        0
    );
    social(
        &app,
        &bob,
        json!({"action":"accept","account_id":alice_id}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        snapshot(&mut pending_again, |s| s["friends"][0]["relationship"]
            == "friend")
        .await["friends"][0]["unread_count"],
        0
    );
    pending_again.close(None).await.unwrap();
    other_device.close(None).await.ok();
    a.close(None).await.ok();
    b.close(None).await.ok();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn requests_private_history_calls_presence_and_revocation() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (alice_id, alice) = user(&db, "friend_alice");
    let (bob_id, bob) = user(&db, "friend_bob");
    let (_, outsider) = user(&db, "friend_outsider");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let mut a = socket(address, &alice).await;
    let mut b = socket(address, &bob).await;
    assert!(
        snapshot(&mut a, |_| true).await["friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    social(
        &app,
        "",
        json!({"action":"search","query":"friend"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"search","query":"%"}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let search = social(
        &app,
        &alice,
        json!({"action":"search","query":"friend_b"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(search["people"].as_array().unwrap().len(), 1);
    assert!(search["people"][0].get("email").is_none());
    assert!(search["people"][0].get("identities").is_none());
    social(
        &app,
        &alice,
        json!({"action":"add","account_id":alice_id}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"add","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    let pending = snapshot(&mut b, |s| s["friends"][0]["relationship"] == "incoming").await;
    assert_eq!(
        pending["friends"][0]["online"], false,
        "requests must not disclose presence"
    );
    let conversation = pending["friends"][0]["conversation_id"].clone();
    social(
        &app,
        &alice,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"accept","account_id":bob_id}),
        StatusCode::CONFLICT,
    )
    .await;
    social(
        &app,
        &bob,
        json!({"action":"accept","account_id":alice_id}),
        StatusCode::OK,
    )
    .await;
    let accepted = snapshot(&mut a, |s| s["friends"][0]["relationship"] == "friend").await;
    assert_eq!(accepted["friends"][0]["online"], true);
    event(
        &mut b,
        json!({"type":"select","conversation_id":conversation}),
    )
    .await;
    snapshot(&mut b, |s| s["conversation_id"] == conversation).await;
    let command = json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"Private <script>plain text</script>"});
    let (one, two) = tokio::join!(
        social(&app, &alice, command.clone(), StatusCode::OK),
        social(&app, &alice, command.clone(), StatusCode::OK)
    );
    assert_eq!(one["message"]["id"], two["message"]["id"]);
    snapshot(&mut b, |s| {
        s["history"]["messages"]
            .as_array()
            .is_some_and(|m| m.len() == 1)
    })
    .await;
    let mut conflict = command.clone();
    conflict["content"] = json!("changed");
    social(&app, &alice, conflict, StatusCode::CONFLICT).await;
    social(
        &app,
        &outsider,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(&app, &outsider, command.clone(), StatusCode::FORBIDDEN).await;
    social(&app,&alice,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"x".repeat(4001)}),StatusCode::BAD_REQUEST).await;
    social(&app,&bob,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"reply"}),StatusCode::OK).await;
    let latest = social(
        &app,
        &alice,
        json!({"action":"history","conversation_id":conversation,"limit":1}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(latest["history"]["messages"][0]["content"], "reply");
    let older=social(&app,&alice,json!({"action":"history","conversation_id":conversation,"limit":1,"before":latest["history"]["older"]}),StatusCode::OK).await;
    assert_eq!(older["history"]["messages"][0]["id"], one["message"]["id"]);
    let started = social(
        &app,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await;
    let call_id = started["call"]["id"].clone();
    social(
        &app,
        &outsider,
        json!({"action":"accept_call","call_id":call_id}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"accept_call","call_id":call_id}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
        StatusCode::CONFLICT,
    )
    .await;
    social(
        &app,
        &bob,
        json!({"action":"accept_call","call_id":call_id}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut a, |s| s["calls"][0]["state"] == "active").await;
    social(
        &app,
        &bob,
        json!({"action":"end_call","call_id":call_id}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut a, |s| {
        s["calls"].as_array().is_some_and(|c| c.is_empty())
    })
    .await;
    // An additional device must keep Bob online after one socket closes.
    let mut b2 = socket(address, &bob).await;
    snapshot(&mut b2, |_| true).await;
    b.close(None).await.unwrap();
    event(&mut a, json!({"type":"select","conversation_id":null})).await;
    assert_eq!(
        snapshot(&mut a, |_| true).await["friends"][0]["online"],
        true
    );
    b2.close(None).await.unwrap();
    snapshot(&mut a, |s| s["friends"][0]["online"] == false).await;
    social(
        &app,
        &alice,
        json!({"action":"remove","account_id":bob_id}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &bob,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &bob,
        json!({"action":"start_call","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Retrying an old, already committed message still requires current friendship.
    social(&app, &alice, command, StatusCode::FORBIDDEN).await;
    let mut reconnected = socket(address, &alice).await;
    assert!(
        snapshot(&mut reconnected, |_| true).await["friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    reconnected.close(None).await.unwrap();
    server.abort();
}
