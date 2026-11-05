use super::*;

async fn observer(address: std::net::SocketAddr, token: &str, guild: &Value) -> Socket {
    let mut req = format!("ws://{address}/api/v1/socket")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    socket
        .send(Message::Text(
            json!({"version":1,"event":{"type":"connect","token":token}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket.send(Message::Text(json!({"version":1,"event":{"type":"subscribe","subscription":1,"guild_id":guild,"channel_id":null,"voice_roster":true}}).to_string().into())).await.unwrap();
    socket
}

async fn roster(socket: &mut Socket) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["type"], "revoked");
            assert_ne!(frame["event"]["type"], "error");
            if frame["event"]["type"] == "update"
                && frame["event"]["event"]["type"] == "voice_roster"
            {
                assert_eq!(frame["event"]["subscription"], 1);
                return frame["event"]["event"]["channels"].clone();
            }
        }
    })
    .await
    .expect("voice roster did not update")
}

async fn revoked_without_roster(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["event"]["type"], "voice_roster");
            if frame["event"]["type"] == "revoked" {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn visible_rosters_without_join_permission_follow_membership_and_revoke() {
    let db = database();
    let (owner, ot) = user(&db, "roster_owner");
    let (viewer, vt) = user(&db, "roster_viewer");
    let (_, outsider) = user(&db, "roster_outsider");
    permissions::bootstrap_owner(&db.pool, "roster_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Roster test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"roster_viewer"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"visible","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"hidden","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let hidden = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hidden")
        .unwrap()["id"]
        .clone();
    for (id, denied) in [(&channel, "join_voice"), (&hidden, "view_channel")] {
        change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":id,"target":{"kind":"member","id":viewer},"allow":[],"deny":[denied]}), StatusCode::OK).await;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut watching = observer(address, &vt, &guild).await;
    assert_eq!(
        roster(&mut watching).await,
        json!([{"channel_id":channel,"members":[]}])
    );

    let mut peer = Peer::new(join(address, &ot, guild.clone(), channel.clone()).await).await;
    let joined = roster(&mut watching).await;
    assert_eq!(joined.as_array().unwrap().len(), 1);
    assert_eq!(joined[0]["members"][0]["account_id"], owner.to_string());
    assert_eq!(joined[0]["members"][0]["display_name"], "roster_owner");
    send(
        &mut peer.socket,
        ClientEvent::State {
            muted: true,
            deafened: true,
        },
    )
    .await;
    let muted = roster(&mut watching).await;
    assert_eq!(muted[0]["members"][0]["muted"], true);
    assert_eq!(muted[0]["members"][0]["deafened"], true);
    peer.close().await;
    assert_eq!(
        roster(&mut watching).await,
        json!([{"channel_id":channel,"members":[]}])
    );

    // No implicit guild access, even for authenticated accounts.
    let mut denied = observer(address, &outsider, &guild).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let Message::Text(text) = denied.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["event"]["type"], "voice_roster");
            if frame["event"]["event"]["type"] == "error" {
                break;
            }
        }
    })
    .await
    .unwrap();
    // Steady-state ticks reuse cached visibility; a commit within that window
    // must still revoke before any roster built from the old grants is sent.
    change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":viewer},"allow":[],"deny":["view_channel"]}), StatusCode::OK).await;
    revoked_without_roster(&mut watching).await;
    let _ = watching.close(None).await;
    let mut watching = observer(address, &vt, &guild).await;
    assert_eq!(roster(&mut watching).await, json!([]));
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"remove_member","account_id":viewer}),
        StatusCode::OK,
    )
    .await;
    revoked_without_roster(&mut watching).await;
    let _ = watching.close(None).await;
    let _ = denied.close(None).await;
    serving.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn transient_database_errors_keep_admitted_voice_users_connected() {
    let db = database();
    let (_, ot) = user(&db, "transient_owner");
    let (guest_id, gt) = user(&db, "transient_guest");
    permissions::bootstrap_owner(&db.pool, "transient_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Transient test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"transient_guest"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut owner = Peer::new(join(address, &ot, guild.clone(), channel.clone()).await).await;
    let mut guest = Peer::new(join(address, &gt, guild.clone(), channel.clone()).await).await;
    forwarded(&mut owner, &mut guest).await;
    // Hold the guild row: every full voice check now hits the one-second lock
    // timeout, a database error that says nothing about authorization.
    let mut blocker = db.pool.get().unwrap();
    blocker
        .batch_execute(&format!(
            "BEGIN; SELECT id FROM guilds WHERE id='{}' FOR UPDATE",
            guild.as_str().unwrap()
        ))
        .unwrap();
    // Past the ten-second recheck, both stay connected and media still flows
    // on their current, unrevoked access epochs.
    let until = std::time::Instant::now() + Duration::from_secs(13);
    while std::time::Instant::now() < until {
        still_connected(&mut owner.socket).await;
        still_connected(&mut guest.socket).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    forwarded(&mut owner, &mut guest).await;
    blocker.batch_execute("ROLLBACK").unwrap();
    drop(blocker);
    // Rechecks succeed again once the database recovers.
    tokio::time::sleep(Duration::from_secs(2)).await;
    still_connected(&mut guest.socket).await;
    forwarded(&mut guest, &mut owner).await;
    // Between rechecks the roster reuses the last result; an access commit still
    // revokes immediately and leaves the other participant connected.
    change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["join_voice"]}), StatusCode::OK).await;
    loop {
        match event(&mut guest.socket).await {
            ServerEvent::Revoked {} => break,
            ServerEvent::Participants { .. }
            | ServerEvent::MediaDiagnostics { .. }
            | ServerEvent::ScreenTarget { .. }
            | ServerEvent::Pong {} => {}
            _ => panic!("expected revocation"),
        }
    }
    still_connected(&mut owner.socket).await;
    owner.close().await;
    serving.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn session_expiry_inside_the_reuse_window_ends_voice_within_the_recheck() {
    let db = database();
    let (_, ot) = user(&db, "expiry_owner");
    let (guest_id, gt) = user(&db, "expiry_guest");
    permissions::bootstrap_owner(&db.pool, "expiry_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Expiry test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"expiry_guest"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut owner = Peer::new(join(address, &ot, guild.clone(), channel.clone()).await).await;
    let mut guest = Peer::new(join(address, &gt, guild.clone(), channel.clone()).await).await;
    forwarded(&mut owner, &mut guest).await;
    // Expiry commits no access change: the ten-second recheck must find it.
    diesel::sql_query("UPDATE sessions SET expires_at=now() WHERE account_id=$1::uuid")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            match event(&mut guest.socket).await {
                ServerEvent::Revoked {} => break,
                ServerEvent::Error { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("an expired session must lose voice within the recheck");
    still_connected(&mut owner.socket).await;
    owner.close().await;
    serving.abort();
}
