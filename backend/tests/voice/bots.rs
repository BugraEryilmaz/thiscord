use super::*;
use thiscord_shared::voice_bots::{PATH, Settings};

async fn bots(
    app: &Router,
    token: &str,
    guild: &Value,
    channel: &Value,
    action: Value,
    expected: StatusCode,
) -> Value {
    let (status, body) = call(
        app,
        PATH,
        token,
        json!({"guild_id":guild,"channel_id":channel,"command":action}),
    )
    .await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn roster(peer: &mut Peer, count: usize) -> Vec<voice::Participant> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let ServerEvent::Participants { members } = event(&mut peer.socket).await
                && members.iter().filter(|p| p.test_bot).count() == count
            {
                return members;
            }
        }
    })
    .await
    .expect("bot roster converges")
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn owner_bots_real_audio_controls_capacity_isolation_and_revocation() {
    let db = database();
    let (_, owner) = user(&db, "bot_owner");
    let (admin_id, admin) = user(&db, "bot_admin");
    let (_, member) = user(&db, "bot_member");
    permissions::bootstrap_owner(&db.pool, "bot_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    command(
        &app,
        &owner,
        json!({"action":"set_instance_admin","account_id":admin_id,"admin":true}),
        StatusCode::OK,
    )
    .await;
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Bot test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    for username in ["bot_admin", "bot_member"] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    for (name, kind) in [("voice", "voice"), ("isolated", "voice"), ("text", "text")] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"create_channel","name":name,"kind":kind}),
            StatusCode::OK,
        )
        .await;
    }
    let channel = |name| {
        state["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()["id"]
            .clone()
    };
    let voice = channel("voice");
    let isolated = channel("isolated");
    let text = channel("text");
    let defaults = serde_json::to_value(Settings::default()).unwrap();
    for token in [&admin, &member] {
        for action in [
            json!({"action":"list"}),
            json!({"action":"add","settings":defaults}),
        ] {
            bots(&app, token, &guild, &voice, action, StatusCode::FORBIDDEN).await;
        }
    }
    bots(
        &app,
        "invalid",
        &guild,
        &voice,
        json!({"action":"list"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    bots(
        &app,
        &owner,
        &guild,
        &text,
        json!({"action":"add","settings":defaults}),
        StatusCode::FORBIDDEN,
    )
    .await;
    // An instance owner still needs guild membership and voice access.
    let mut other = command(
        &app,
        &admin,
        json!({"action":"create_guild","name":"Private"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &admin,
        &mut other,
        json!({"action":"create_channel","name":"private","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    bots(
        &app,
        &owner,
        &other["guild"]["id"],
        &other["channels"][0]["id"],
        json!({"action":"add","settings":defaults}),
        StatusCode::FORBIDDEN,
    )
    .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    // Start with an empty channel: bot admission must not require a human peer.
    let first = bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"add","settings":defaults}),
        StatusCode::OK,
    )
    .await["bots"][0]
        .clone();
    let mut receiver = Peer::new(join(addr, &member, guild.clone(), voice.clone()).await).await;
    let mut separate = Peer::new(join(addr, &admin, guild.clone(), isolated.clone()).await).await;
    let members = roster(&mut receiver, 1).await;
    let slot = members.iter().find(|p| p.test_bot).unwrap().slot;
    let packet = tokio::time::timeout(Duration::from_secs(8), receiver.packets.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(packet.header.ssrc, voice::SSRC_BASE + slot as u32);
    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Mono).unwrap();
    let mut samples = [0.0; 5760];
    assert_eq!(
        decoder
            .decode_float(&packet.payload, &mut samples, false)
            .unwrap(),
        960
    );
    assert!(separate.packets.try_recv().is_err());

    // Receive paths decode real user audio, and per-bot controls are revision fenced.
    for _ in 0..10 {
        receiver.publish().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let listed = bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    assert!(listed["bots"][0]["received_packets"].as_u64().unwrap() > 0);
    let mut muted = defaults.clone();
    muted["muted"] = json!(true);
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"update","bot_id":first["id"],"revision":0,"settings":muted}),
        StatusCode::OK,
    )
    .await;
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"update","bot_id":first["id"],"revision":0,"settings":defaults}),
        StatusCode::CONFLICT,
    )
    .await;
    bots(
        &app,
        &owner,
        &guild,
        &isolated,
        json!({"action":"remove","bot_id":first["id"]}),
        StatusCode::NOT_FOUND,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if roster(&mut receiver, 1)
                .await
                .iter()
                .any(|p| p.test_bot && p.muted)
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    while receiver.packets.try_recv().is_ok() {}
    assert!(
        tokio::time::timeout(Duration::from_millis(300), receiver.packets.recv())
            .await
            .is_err()
    );
    let mut bad = defaults.clone();
    bad["input_gain"] = json!(99);
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"update","bot_id":first["id"],"revision":1,"settings":bad}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"update","bot_id":first["id"],"revision":1,"settings":defaults}),
        StatusCode::OK,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), receiver.packets.recv())
        .await
        .unwrap()
        .unwrap();

    let mut ids = vec![first["id"].clone()];
    for index in 1..7 {
        let mut settings = defaults.clone();
        settings["microphone_loop"] = json!(if index % 2 == 0 { "pulse" } else { "melody" });
        settings["profile"] = json!(if index % 2 == 0 {
            "low_bandwidth"
        } else {
            "high_loss"
        });
        let result = bots(
            &app,
            &owner,
            &guild,
            &voice,
            json!({"action":"add","settings":settings}),
            StatusCode::OK,
        )
        .await;
        ids = result["bots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["id"].clone())
            .collect();
        assert_eq!(ids.len(), index + 1);
    }
    assert_eq!(roster(&mut receiver, 7).await.len(), 8);
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"add","settings":defaults}),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"remove","bot_id":ids[0]}),
        StatusCode::OK,
    )
    .await;
    roster(&mut receiver, 6).await;
    // Transfer closes all old-owner media before its role change can commit.
    command(
        &app,
        &owner,
        json!({"action":"transfer_instance","account_id":admin_id}),
        StatusCode::OK,
    )
    .await;
    roster(&mut receiver, 0).await;
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"list"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let empty = bots(
        &app,
        &admin,
        &guild,
        &voice,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    assert!(empty["bots"].as_array().unwrap().is_empty());
    bots(
        &app,
        &admin,
        &guild,
        &voice,
        json!({"action":"add","settings":defaults}),
        StatusCode::OK,
    )
    .await;
    roster(&mut receiver, 1).await;
    // Instance ownership never bypasses a channel Speak denial.
    change(&app, &owner, &mut state, json!({"action":"set_override","channel_id":voice,"target":{"kind":"member","id":admin_id},"allow":[],"deny":["speak"]}), StatusCode::OK).await;
    roster(&mut receiver, 0).await;
    bots(
        &app,
        &admin,
        &guild,
        &voice,
        json!({"action":"add","settings":defaults}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(&app, &owner, &mut state, json!({"action":"delete_override","channel_id":voice,"target":{"kind":"member","id":admin_id}}), StatusCode::OK).await;
    bots(
        &app,
        &admin,
        &guild,
        &voice,
        json!({"action":"add","settings":defaults}),
        StatusCode::OK,
    )
    .await;
    roster(&mut receiver, 1).await;
    let (status, _) = call(&app, ACCOUNT_PATH, &admin, json!({"action":"logout"})).await;
    assert_eq!(status, StatusCode::OK);
    roster(&mut receiver, 0).await;
    receiver.close().await;
    separate.close().await;
    serving.abort();
}
