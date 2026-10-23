use super::*;
use thiscord_shared::{account::ACCOUNT_PATH, bots::BOTS_PATH};

async fn manage(app: &Router, token: &str, request: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, BOTS_PATH, token, request).await;
    assert_eq!(
        status,
        expected,
        "{}",
        body.get("code").unwrap_or(&Value::Null)
    );
    body
}
async fn open(address: std::net::SocketAddr, token: &str, channels: Value) -> Socket {
    let (mut socket, _) = connect_async(format!(
        "ws://{address}{}",
        thiscord_shared::bots::SOCKET_PATH
    ))
    .await
    .unwrap();
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"authenticate","token":token,"channels":channels}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
}
// Pong is an ordering fence for queued events; any mention before it is a failure.
async fn no_mentions(socket: &mut Socket) {
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"ping"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let WsMessage::Text(text) = socket.next().await.unwrap().unwrap() {
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["event"]["type"], "pong", "Unexpected bot event");
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn scripting_bots_isolate_credentials_and_deliver_only_live_human_mentions() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    manage(&app,&guest,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"music_bot"}),StatusCode::FORBIDDEN).await;
    let issued=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"music_bot"}),StatusCode::OK).await;
    let bot = issued["bot"]["account_id"].clone();
    let token = issued["token"].as_str().unwrap();
    assert!(token.starts_with("bot_") && token.len() == 47);
    #[derive(QueryableByName)]
    struct Stored {
        #[diesel(sql_type=diesel::sql_types::Bool)]
        valid: bool,
    }
    let stored=diesel::sql_query("SELECT a.is_bot AND NOT a.email_verified AND a.email=$1||'@bots.invalid' AND NOT EXISTS(SELECT 1 FROM identities WHERE account_id=a.id) AND EXISTS(SELECT 1 FROM sessions s JOIN session_tokens t ON t.session_id=s.id WHERE s.account_id=a.id AND t.token_hash=$2) AS valid FROM accounts a WHERE a.id=$1::uuid")
        .bind::<Text,_>(bot.as_str().unwrap()).bind::<Text,_>(URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes())))
        .get_result::<Stored>(&mut db.pool.get().unwrap()).unwrap();
    assert!(stored.valid);
    state["guild"]["revision"] = issued["revision"].clone();
    // Nonhuman accounts cannot strand guild/instance ownership or gain an
    // instance role; every administrative assignment requires a verified human.
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"transfer_owner","account_id":bot}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &owner,
        json!({"action":"set_instance_admin","account_id":bot,"admin":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &owner,
        json!({"action":"transfer_instance","account_id":bot}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let listed = manage(
        &app,
        &owner,
        json!({"action":"list","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(listed["bots"][0]["account_id"], bot);
    assert!(listed.get("token").is_none());
    for credential in [token, &token[4..]] {
        for (path, request) in [
            (ACCOUNT_PATH, json!({"action":"current"})),
            (PERMISSIONS_PATH, json!({"action":"instance"})),
            (
                thiscord_shared::safety::SAFETY_PATH,
                json!({"action":"blocks"}),
            ),
            (
                thiscord_shared::search::SEARCH_PATH,
                json!({"action":"search","filters":{"scope":{"kind":"guild","guild_id":guild,"channel_id":channel},"query":"needle"}}),
            ),
            (
                thiscord_shared::friends::FRIENDS_PATH,
                json!({"action":"search","query":"owner"}),
            ),
            (BOTS_PATH, json!({"action":"list","guild_id":guild})),
        ] {
            assert_eq!(
                call(&app, path, credential, request).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(thiscord_shared::admin::DIAGNOSTICS_PATH)
                    .header("authorization", format!("Bearer {credential}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }
    let sent=chat(&app,token,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"A bot response"}),StatusCode::OK).await;
    assert_eq!(sent["message"]["author_id"], bot);
    assert_eq!(sent["message"]["display_name"], "music_bot [bot]");
    chat(&app,&token[4..],json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"stripped"}),StatusCode::UNAUTHORIZED).await;
    // An old mention must never become a command after connecting.
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"@music_bot /old"}),StatusCode::OK).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut socket = open(address, token, json!([channel])).await;
    assert_eq!(event(&mut socket, "ready").await["bot"]["account_id"], bot);
    no_mentions(&mut socket).await;
    let request = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"@music_bot /play String args"});
    let message = chat(&app, &owner, request.clone(), StatusCode::OK).await["message"].clone();
    assert_eq!(
        event(&mut socket, "mention").await["message"]["id"],
        message["id"]
    );
    chat(&app, &owner, request, StatusCode::OK).await;
    chat(&app,&owner,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":0,"content":"@music_bot /edited"}),StatusCode::OK).await;
    chat(&app,token,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"@music_bot /loop"}),StatusCode::OK).await;
    chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"not a mention"}),StatusCode::OK).await;
    no_mentions(&mut socket).await;
    let mut invalid = open(address, &owner, json!([channel])).await;
    event(&mut invalid, "error").await;
    let mut invalid = open(address, token, json!([])).await;
    event(&mut invalid, "error").await;
    let rotated = manage(
        &app,
        &owner,
        json!({"action":"rotate","guild_id":guild,"revision":issued["revision"],"account_id":bot}),
        StatusCode::OK,
    )
    .await;
    event(&mut socket, "revoked").await;
    let replacement = rotated["token"].as_str().unwrap();
    chat(
        &app,
        token,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    chat(
        &app,
        replacement,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    let mut socket = open(address, replacement, json!([channel])).await;
    event(&mut socket, "ready").await;
    state["guild"]["revision"] = rotated["revision"].clone();
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":bot},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    event(&mut socket, "revoked").await;
    chat(&app,replacement,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"denied"}),StatusCode::FORBIDDEN).await;
    let mut denied = open(address, replacement, json!([channel])).await;
    event(&mut denied, "error").await;
    manage(&app,&owner,json!({"action":"delete","guild_id":guild,"revision":state["guild"]["revision"],"account_id":bot}),StatusCode::OK).await;
    chat(
        &app,
        replacement,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn scripting_bots_creator_deletion_guild_cleanup_and_member_capacity() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Manager","position":1,"permissions":["manage_guild"]}),StatusCode::OK).await;
    let role = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Manager")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":guest_id,"role_id":role,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    let issued=manage(&app,&guest,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"persistent_bot"}),StatusCode::OK).await;
    let token = issued["token"].as_str().unwrap();
    let (status, _) = call(
        &app,
        ACCOUNT_PATH,
        &guest,
        json!({"action":"delete_account","confirmation":"guest"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    chat(
        &app,
        token,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    state = command(
        &app,
        &owner,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    // Bots consume the ordinary owner-based member allocation too.
    diesel::sql_query("UPDATE instance_role_limits SET members=2 WHERE role='owner'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let full=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"too_many_members"}),StatusCode::CONFLICT).await;
    assert_eq!(full["code"], "limit_exceeded");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut socket = open(address, token, json!([channel])).await;
    event(&mut socket, "ready").await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"delete"}),
        StatusCode::OK,
    )
    .await;
    event(&mut socket, "revoked").await;
    chat(
        &app,
        token,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type=diesel::sql_types::BigInt)]
        count: i64,
    }
    assert_eq!(
        diesel::sql_query("SELECT count(*) FROM accounts WHERE is_bot")
            .get_result::<Count>(&mut db.pool.get().unwrap())
            .unwrap()
            .count,
        0
    );
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn scripting_bots_scope_roles_quotas_and_rollback() {
    use diesel_migrations::MigrationHarness;
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    // A distinct guild's owner/instance administrator has no implicit access.
    let (_, outsider) = user(&db, "outside");
    let (admin_id, admin) = user(&db, "instance_admin");
    command(
        &app,
        &owner,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":true}),
        StatusCode::OK,
    )
    .await;
    manage(
        &app,
        &admin,
        json!({"action":"list","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let other = command(
        &app,
        &outsider,
        json!({"action":"create_guild","name":"other"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    manage(
        &app,
        &outsider,
        json!({"action":"list","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let issued=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"scoped_bot"}),StatusCode::OK).await;
    let bot = issued["bot"]["account_id"].clone();
    let token = issued["token"].as_str().unwrap();
    state["guild"]["revision"] = issued["revision"].clone();
    chat(
        &app,
        token,
        json!({"action":"unread","guild_id":other["guild"]["id"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    manage(
        &app,
        &owner,
        json!({"action":"rotate","guild_id":guild,"revision":0,"account_id":bot}),
        StatusCode::CONFLICT,
    )
    .await;
    // Appropriate guild managers can create bots, but cannot rotate higher roles.
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Bot manager","position":1,"permissions":["manage_guild"]}),StatusCode::OK).await;
    let manager = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Bot manager")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":guest_id,"role_id":manager,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    let by_manager=manage(&app,&guest,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"manager_bot"}),StatusCode::OK).await;
    state["guild"]["revision"] = by_manager["revision"].clone();
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    manage(&app,&guest,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"escalation_bot"}),StatusCode::FORBIDDEN).await;
    manage(&app,&guest,json!({"action":"rotate","guild_id":guild,"revision":state["guild"]["revision"],"account_id":by_manager["bot"]["account_id"]}),StatusCode::FORBIDDEN).await;
    chat(
        &app,
        by_manager["token"].as_str().unwrap(),
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    change(&app,&owner,&mut state,json!({"action":"delete_override","channel_id":channel,"target":{"kind":"member","id":guest_id}}),StatusCode::OK).await;
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Higher bot","position":2,"permissions":["send_messages"]}),StatusCode::OK).await;
    let higher = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Higher bot")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":bot,"role_id":higher,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    manage(&app,&guest,json!({"action":"rotate","guild_id":guild,"revision":state["guild"]["revision"],"account_id":bot}),StatusCode::FORBIDDEN).await;
    // Installed accounts cannot be added to a second guild by ordinary APIs.
    let mut other = other;
    change(
        &app,
        &outsider,
        &mut other,
        json!({"action":"add_member","username":"scoped_bot"}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    // Credentials survive idle time, but never membership removal.
    diesel::sql_query(
        "UPDATE sessions SET last_seen_at=now()-interval '20 days' WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(bot.as_str().unwrap())
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
    chat(
        &app,
        token,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":bot}),
        StatusCode::OK,
    )
    .await;
    chat(
        &app,
        token,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    // Removed installations still consume the bot ceiling until deleted.
    for index in 0..8 {
        let added=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":format!("limit_bot_{index}")}),StatusCode::OK).await;
        state["guild"]["revision"] = added["revision"].clone();
    }
    let full=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"over_limit"}),StatusCode::CONFLICT).await;
    assert_eq!(full["code"], "limit_exceeded");
    // Rollback removes bot accounts and sessions while retaining human accounts.
    let mut c = db.pool.get().unwrap();
    c.revert_last_migration(db::MIGRATIONS).unwrap();
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type=diesel::sql_types::BigInt)]
        count: i64,
    }
    assert_eq!(
        diesel::sql_query(
            "SELECT count(*) FROM accounts WHERE username IN ('scoped_bot','manager_bot')"
        )
        .get_result::<Count>(&mut c)
        .unwrap()
        .count,
        0
    );
    assert_eq!(
        diesel::sql_query("SELECT count(*) FROM accounts WHERE username='owner'")
            .get_result::<Count>(&mut c)
            .unwrap()
            .count,
        1
    );
    c.run_pending_migrations(db::MIGRATIONS).unwrap();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn scripting_bots_rotation_compares_grants_after_timeout_expiry() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Manager","position":3,"permissions":["manage_guild"]}),StatusCode::OK).await;
    let manager = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Manager")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":guest_id,"role_id":manager,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    let issued=manage(&app,&owner,json!({"action":"create","guild_id":guild,"revision":state["guild"]["revision"],"name":"moderator_bot"}),StatusCode::OK).await;
    let bot = issued["bot"]["account_id"].clone();
    let token = issued["token"].as_str().unwrap();
    state["guild"]["revision"] = issued["revision"].clone();
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Moderator","position":1,"permissions":["manage_messages"]}),StatusCode::OK).await;
    let moderator = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Moderator")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":bot,"role_id":moderator,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    manage(&app,&guest,json!({"action":"rotate","guild_id":guild,"revision":state["guild"]["revision"],"account_id":bot}),StatusCode::FORBIDDEN).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"timeout_member","account_id":bot,"duration_seconds":600}),
        StatusCode::OK,
    )
    .await;
    state = command(
        &app,
        &owner,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    // The bot now appears weaker than the issuer, but its durable token will
    // regain ManageMessages after the temporary restriction expires.
    manage(&app,&guest,json!({"action":"rotate","guild_id":guild,"revision":state["guild"]["revision"],"account_id":bot}),StatusCode::FORBIDDEN).await;
    let message=chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"moderation target"}),StatusCode::OK).await["message"].clone();
    let delete = json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":0});
    chat(&app, token, delete.clone(), StatusCode::FORBIDDEN).await;
    diesel::sql_query("UPDATE guild_moderation SET timeout_until=now()-interval '1 second' WHERE guild_id=$1::uuid AND account_id=$2::uuid").bind::<Text,_>(guild.as_str().unwrap()).bind::<Text,_>(bot.as_str().unwrap()).execute(&mut db.pool.get().unwrap()).unwrap();
    manage(&app,&guest,json!({"action":"rotate","guild_id":guild,"revision":state["guild"]["revision"],"account_id":bot}),StatusCode::FORBIDDEN).await;
    chat(&app, &guest, delete.clone(), StatusCode::FORBIDDEN).await;
    chat(&app, token, delete, StatusCode::OK).await;
}
