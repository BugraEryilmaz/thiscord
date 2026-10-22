use super::*;
use thiscord_shared::notifications::{PATH, SOCKET_PATH};

async fn notification(app: &Router, token: &str, request: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, PATH, token, request).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn socket(address: std::net::SocketAddr, token: &str, kinds: Value) -> Socket {
    let mut request = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    event(
        &mut socket,
        json!({"type":"connect","token":token,"kinds":kinds}),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], "ready");
    socket
}
async fn event(socket: &mut Socket, event: Value) {
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":event}).to_string().into(),
        ))
        .await
        .unwrap();
}
async fn next(socket: &mut Socket) -> Value {
    loop {
        let frame = next_frame(socket).await;
        if frame["type"] == "calls" {
            continue;
        }
        if frame["type"] == "ready" {
            assert_eq!(next_frame(socket).await["type"], "calls");
        }
        return frame;
    }
}
async fn next_frame(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(4), socket.next())
        .await
        .expect("notification arrived")
        .unwrap()
        .unwrap();
    let WsMessage::Text(text) = message else {
        panic!("expected frame")
    };
    serde_json::from_str::<Value>(&text).unwrap()["event"].clone()
}
async fn quiet(socket: &mut Socket) {
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), socket.next())
            .await
            .is_err(),
        "unexpected notification"
    );
}
async fn server(app: &Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = app.clone();
    (
        address,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn notifications_push_across_channels_deduplicate_filter_mute_and_revoke() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let (_, outsider) = user(&db, "notification_outsider");
    let guild = state["guild"]["id"].clone();
    let target = json!({"type":"channel","guild_id":guild,"channel_id":channel});
    let (address, server) = server(&app).await;
    let kinds = json!(["guild_message"]);
    let mut guest_socket = socket(address, &guest, kinds.clone()).await;
    let mut other_device = socket(address, &guest, kinds.clone()).await;
    let mut owner_socket = socket(address, &owner, kinds.clone()).await;
    let mut foreign = socket(address, &outsider, kinds.clone()).await;
    let request = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"new message"});
    let sent = chat(&app, &owner, request.clone(), StatusCode::OK).await;
    let alert = next(&mut guest_socket).await["alert"].clone();
    assert_eq!(alert["id"], sent["message"]["id"]);
    assert_eq!(alert["delivery"]["sound"], true);
    assert_eq!(alert["delivery"]["banner"], false);
    assert_eq!(next(&mut other_device).await["alert"]["id"], alert["id"]);
    quiet(&mut owner_socket).await;
    quiet(&mut foreign).await;
    chat(&app, &owner, request, StatusCode::OK).await;
    chat(&app,&owner,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":sent["message"]["id"],"revision":0,"content":"edited"}),StatusCode::OK).await;
    quiet(&mut guest_socket).await;
    notification(
        &app,
        &outsider,
        json!({"action":"snooze","target":target,"duration":"forever"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let prefs = notification(
        &app,
        &guest,
        json!({"action":"snooze","target":target,"duration":"minutes15"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(prefs["subscriptions"][0]["mute"]["type"], "until");
    assert_eq!(next(&mut guest_socket).await["type"], "ready");
    assert_eq!(next(&mut other_device).await["type"], "ready");
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"muted"}),StatusCode::OK).await;
    quiet(&mut guest_socket).await;
    let prefs = notification(
        &app,
        &guest,
        json!({"action":"snooze","target":target,"duration":"resume"}),
        StatusCode::OK,
    )
    .await;
    next(&mut guest_socket).await;
    next(&mut other_device).await;
    let delivery = json!({"mode":"mentions","sound":false,"banner":true,"volume":35});
    notification(
        &app,
        &guest,
        json!({"action":"set_subscription","revision":0,"target":target,"delivery":delivery}),
        StatusCode::CONFLICT,
    )
    .await;
    notification(&app,&guest,json!({"action":"set_subscription","revision":prefs["revision"],"target":target,"delivery":delivery}),StatusCode::OK).await;
    next(&mut guest_socket).await;
    next(&mut other_device).await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"without mention"}),StatusCode::OK).await;
    quiet(&mut guest_socket).await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"hello @guest"}),StatusCode::OK).await;
    assert_eq!(next(&mut guest_socket).await["alert"]["delivery"], delivery);
    next(&mut other_device).await;
    event(&mut guest_socket, json!({"type":"subscribe","kinds":[]})).await;
    next(&mut guest_socket).await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"@guest not subscribed"}),StatusCode::OK).await;
    quiet(&mut guest_socket).await;
    next(&mut other_device).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"after removal"}),StatusCode::OK).await;
    quiet(&mut other_device).await;
    // Preferences survive reconnect even though access to the source was revoked.
    let saved = notification(&app, &guest, json!({"action":"get"}), StatusCode::OK).await;
    assert_eq!(saved["subscriptions"][0]["delivery"], delivery);
    let saved = notification(
        &app,
        &guest,
        json!({"action":"remove_subscription","revision":saved["revision"],"target":target}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["subscriptions"], json!([]));
    notification(
        &app,
        &guest,
        json!({"action":"snooze","target":target,"duration":"forever"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn notifications_private_calls_release_and_expired_snooze() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (alice_id, alice) = user(&db, "alert_alice");
    let (bob_id, bob) = user(&db, "alert_bob");
    permissions::bootstrap_owner(&db.pool, "alert_alice").unwrap();
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(alice_id.to_string()).bind::<Text,_>(bob_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = server(&app).await;
    let mut socket = socket(
        address,
        &bob,
        json!(["direct_message", "incoming_call", "voice_leave", "update"]),
    )
    .await;
    let target = json!({"type":"conversation","conversation_id":conversation});
    let request = json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"private"});
    assert_eq!(
        call(
            &app,
            thiscord_shared::friends::FRIENDS_PATH,
            &alice,
            request.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let alert = next(&mut socket).await["alert"].clone();
    assert_eq!(alert["delivery"]["sound"], true);
    assert_eq!(alert["delivery"]["banner"], true);
    call(
        &app,
        thiscord_shared::friends::FRIENDS_PATH,
        &alice,
        request,
    )
    .await;
    quiet(&mut socket).await;
    let (status, started) = call(
        &app,
        thiscord_shared::friends::FRIENDS_PATH,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        next(&mut socket).await["alert"]["call_id"],
        started["call"]["id"]
    );
    let (status, _) = call(
        &app,
        thiscord_shared::friends::FRIENDS_PATH,
        &alice,
        json!({"action":"end_call","call_id":started["call"]["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next(&mut socket).await["alert"]["kind"], "voice_leave");
    notification(
        &app,
        &bob,
        json!({"action":"publish_update","version":"99.0.0"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    notification(
        &app,
        &alice,
        json!({"action":"publish_update","version":"https://bad.test"}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    notification(
        &app,
        &alice,
        json!({"action":"publish_update","version":"99.0.0"}),
        StatusCode::OK,
    )
    .await;
    let update = next(&mut socket).await["alert"].clone();
    assert_eq!(update["kind"], "update");
    assert_eq!(update["delivery"]["sound"], false);
    notification(
        &app,
        &alice,
        json!({"action":"publish_update","version":"99.0.0"}),
        StatusCode::OK,
    )
    .await;
    quiet(&mut socket).await;
    notification(
        &app,
        &bob,
        json!({"action":"snooze","target":target,"duration":"minutes15"}),
        StatusCode::OK,
    )
    .await;
    next(&mut socket).await;
    // Advance only the persisted deadline in this isolated test schema. No timer
    // or client request is needed to resume delivery after the deadline.
    diesel::sql_query("UPDATE notification_preferences SET settings=jsonb_set(settings,'{subscriptions,0,mute,at}','\"2020-01-01T00:00:00Z\"') WHERE account_id=$1::uuid").bind::<Text,_>(bob_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    call(&app,thiscord_shared::friends::FRIENDS_PATH,&alice,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"after expiry"})).await;
    assert_eq!(next(&mut socket).await["alert"]["kind"], "direct_message");
    notification(&app,&bob,json!({"action":"set_default","revision":1,"kind":"direct_message","delivery":{"mode":"all","sound":true,"banner":true,"volume":101}}),StatusCode::BAD_REQUEST).await;
    assert_eq!(
        call(
            &app,
            thiscord_shared::account::ACCOUNT_PATH,
            &bob,
            json!({"action":"logout"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(next(&mut socket).await["type"], "revoked");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn notifications_fanout_keeps_sixteen_subscribers_connected() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, state, channel) = setup(&db).await;
    let (_, outsider) = user(&db, "fanout_outsider");
    let (address, server) = server(&app).await;
    let mut sockets = vec![];
    for _ in 0..16 {
        sockets.push(socket(address, &guest, json!(["guild_message"])).await);
    }
    let mut foreign = socket(address, &outsider, json!(["guild_message"])).await;
    let sent = chat(&app, &owner, json!({"action":"send","guild_id":state["guild"]["id"],"channel_id":channel,"client_id":Uuid::new_v4(),"content":"fanout"}), StatusCode::OK).await;
    for socket in &mut sockets {
        assert_eq!(next(socket).await["alert"]["id"], sent["message"]["id"]);
        event(socket, json!({"type":"ping"})).await;
        assert_eq!(next(socket).await["type"], "pong");
    }
    quiet(&mut foreign).await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn notifications_exclude_calls_expired_while_authorization_waited() {
    let _lock = TEST_LOCK.lock().await;
    let mut db = database();
    prepare_blocking_test(&mut db);
    let (alice_id, alice) = user(&db, "expiry_alice");
    let (bob_id, bob) = user(&db, "expiry_bob");
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(alice_id.to_string()).bind::<Text,_>(bob_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (status, started) = call(
        &app,
        thiscord_shared::friends::FRIENDS_PATH,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (address, server) = server(&app).await;
    db.connection.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM accounts WHERE id=$1::uuid FOR NO KEY UPDATE")
        .bind::<Text, _>(bob_id.to_string())
        .execute(&mut db.connection)
        .unwrap();
    let mut request = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    event(
        &mut socket,
        json!({"type":"connect","token":bob,"kinds":["incoming_call"]}),
    )
    .await;
    // The notification transaction has started but its authorization read is
    // blocked. Expire the call after that transaction's now(), without sleeps.
    wait_for_blocked(&mut db.connection, 1).await;
    diesel::sql_query("UPDATE friend_calls SET expires_at=clock_timestamp() WHERE id=$1::uuid")
        .bind::<Text, _>(started["call"]["id"].as_str().unwrap())
        .execute(&mut db.connection)
        .unwrap();
    db.connection.batch_execute("COMMIT").unwrap();
    assert_eq!(next_frame(&mut socket).await["type"], "ready");
    assert_eq!(
        next_frame(&mut socket).await,
        json!({"type":"calls","ringing":[]})
    );
    quiet(&mut socket).await;
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn notifications_recover_and_clear_calls_without_friends_polling() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (alice_id, alice) = user(&db, "recover_alice");
    let (bob_id, bob) = user(&db, "recover_bob");
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(alice_id.to_string()).bind::<Text,_>(bob_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = server(&app).await;
    let path = thiscord_shared::friends::FRIENDS_PATH;
    let (status, started) = call(
        &app,
        path,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = started["call"]["id"].clone();
    // No notification connection existed at creation. Both devices recover it.
    let mut first = socket(address, &bob, json!(["incoming_call"])).await;
    let mut second = socket(address, &bob, json!(["incoming_call"])).await;
    assert_eq!(next(&mut first).await["alert"]["call_id"], id);
    assert_eq!(next(&mut second).await["alert"]["call_id"], id);
    quiet(&mut first).await;
    // State cleanup must work even after disabling all notification kinds.
    event(&mut second, json!({"type":"subscribe","kinds":[]})).await;
    next(&mut second).await;
    assert_eq!(
        call(
            &app,
            path,
            &bob,
            json!({"action":"accept_call","call_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    for socket in [&mut first, &mut second] {
        assert_eq!(
            next_frame(socket).await,
            json!({"type":"calls","ringing":[]})
        );
        quiet(socket).await;
    }
    assert_eq!(
        call(
            &app,
            path,
            &alice,
            json!({"action":"end_call","call_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    next_frame(&mut first).await;
    next_frame(&mut second).await;
    let (_, started) = call(
        &app,
        path,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await;
    let id = started["call"]["id"].clone();
    assert_eq!(next(&mut first).await["alert"]["call_id"], id);
    assert_eq!(next_frame(&mut second).await["ringing"], json!([id]));
    // Cancel without ever opening the friends socket or observing a polled snapshot.
    assert_eq!(
        call(
            &app,
            path,
            &alice,
            json!({"action":"end_call","call_id":id})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(next_frame(&mut first).await["ringing"], json!([]));
    assert_eq!(next_frame(&mut second).await["ringing"], json!([]));
    let mut after_end = socket(address, &bob, json!(["incoming_call"])).await;
    quiet(&mut after_end).await;
    first.close(None).await.unwrap();
    second.close(None).await.unwrap();
    after_end.close(None).await.unwrap();
    // The expiry is a single scheduled deadline, not a call-state poll.
    let (_, started) = call(
        &app,
        path,
        &alice,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await;
    let id = started["call"]["id"].as_str().unwrap();
    diesel::sql_query(
        "UPDATE friend_calls SET expires_at=now()+interval '2 seconds' WHERE id=$1::uuid",
    )
    .bind::<Text, _>(id)
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
    let mut expiring = socket(address, &bob, json!(["incoming_call"])).await;
    assert_eq!(next(&mut expiring).await["alert"]["call_id"], id);
    assert_eq!(next_frame(&mut expiring).await["ringing"], json!([]));
    let mut expired = socket(address, &bob, json!(["incoming_call"])).await;
    quiet(&mut expired).await;
    server.abort();
}
