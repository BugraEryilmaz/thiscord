use super::*;
use thiscord_shared::voice_bots::{PATH, Settings};

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn owner_bots_rotation_closes_old_voice_without_revoking_restored_session() {
    let db = database();
    let (_, owner) = user(&db, "rotation_owner");
    permissions::bootstrap_owner(&db.pool, "rotation_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Rotation"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":Settings::default()}),
        StatusCode::OK,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut peer = Peer::new(join(addr, &owner, guild.clone(), channel.clone()).await).await;
    roster(&mut peer, 1).await;

    let (status, rotated) = call(&app, ACCOUNT_PATH, &owner, json!({"action":"rotate"})).await;
    assert_eq!(status, StatusCode::OK);
    // Rotation must close the old human connection, just as startup after a
    // crash invalidates its detached voice session. The retained token is not
    // a newly presented credential and must not revoke the replacement token.
    loop {
        if matches!(
            event(&mut peer.socket).await,
            ServerEvent::Revoked {} | ServerEvent::Error { .. }
        ) {
            break;
        }
    }
    let restored = rotated["session"]["token"].as_str().unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed = bots(
            &app,
            restored,
            &guild,
            &channel,
            json!({"action":"list"}),
            StatusCode::OK,
        )
        .await;
        if listed["bots"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "old bot survived rotation"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        call(&app, ACCOUNT_PATH, restored, json!({"action":"current"}))
            .await
            .0,
        StatusCode::OK
    );
    // A new voice handshake presenting that same token is an external replay;
    // ingress must still revoke the session, including its replacement token.
    let mut replay = join(addr, &owner, guild, channel).await;
    assert!(
        matches!(event(&mut replay).await, ServerEvent::Error { error }
        if error.code == thiscord_shared::ErrorCode::Unauthorized)
    );
    assert_eq!(
        call(&app, ACCOUNT_PATH, restored, json!({"action":"current"}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    peer.pc.close().await.unwrap();
    serving.abort();
}

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
async fn audio(app: &Router, token: &str, connection: &Value, command: Value) -> Value {
    let (status, body) = call(
        app,
        thiscord_shared::remote_audio::PATH,
        token,
        json!({"connection_id":connection,"command":command}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("Err").is_none(), "{body}");
    body["Ok"].clone()
}
async fn save_audio(app: &Router, token: &str, connection: &Value, settings: Value) {
    let expected = audio(app, token, connection, json!({"command":"audio_current"})).await;
    audio(
        app,
        token,
        connection,
        json!({"command":"audio_save","settings":settings,"expected":expected}),
    )
    .await;
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

async fn wait_for_no_video(app: &Router, token: &str, guild: &Value, channel: &Value) {
    // Server roster snapshots and bot metrics refresh on separate clocks.
    // Wait for observed convergence rather than assuming a one-second sleep.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed = bots(
            app,
            token,
            guild,
            channel,
            json!({"action":"list"}),
            StatusCode::OK,
        )
        .await;
        if listed["bots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["video"].as_array().unwrap().is_empty())
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "video subscriptions did not close: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn video_frame() -> Vec<u8> {
    use openh264::{
        encoder::{Encoder, EncoderConfig, Profile},
        formats::{RgbSliceU8, YUVBuffer},
    };
    let rgb = vec![120; 320 * 180 * 3];
    let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (320, 180)));
    Encoder::with_api_config(
        openh264::OpenH264API::from_source(),
        EncoderConfig::new().profile(Profile::Baseline),
    )
    .unwrap()
    .encode(&yuv)
    .unwrap()
    .to_vec()
}
async fn publish_video(peer: &mut Peer, frame: &[u8]) {
    use rtp::packetizer::Payloader;
    for kind in [0, 2] {
        let payloads = rtp::codec::h264::H264Payloader::default()
            .payload(1100, &frame.to_vec().into())
            .unwrap();
        let count = payloads.len();
        let timestamp = peer.screen_sequences[kind] as u32 * 9000;
        for (i, payload) in payloads.into_iter().enumerate() {
            peer.screen_sequences[kind] = peer.screen_sequences[kind].wrapping_add(1);
            peer.screen_tracks[kind]
                .write_rtp(rtp::Packet {
                    header: rtp::header::Header {
                        version: 2,
                        payload_type: 125,
                        ssrc: 901 + kind as u32,
                        sequence_number: peer.screen_sequences[kind],
                        timestamp,
                        marker: i + 1 == count,
                        ..Default::default()
                    },
                    payload,
                })
                .await
                .unwrap();
        }
    }
    peer.publish_screen(1).await;
    peer.publish().await;
}

async fn pump_video(peer: &mut Peer, frame: &[u8], duration: Duration) -> (bool, usize) {
    let until = tokio::time::Instant::now() + duration;
    let mut keyframe = false;
    let mut viewers = 0;
    while tokio::time::Instant::now() < until {
        publish_video(peer, frame).await;
        let end = tokio::time::Instant::now() + Duration::from_millis(100);
        while let Ok(event) = tokio::time::timeout_at(end, event(&mut peer.socket)).await {
            match event {
                ServerEvent::ScreenKeyframe { .. } => keyframe = true,
                ServerEvent::Participants { members } => {
                    if let Some(source) = members.iter().find(|m| m.slot == peer.slot) {
                        viewers = viewers
                            .max(source.screen_viewers.len().min(source.camera_viewers.len()));
                    }
                }
                ServerEvent::Error { .. } | ServerEvent::Revoked {} => {
                    panic!("publisher lost access")
                }
                _ => {}
            }
        }
    }
    (keyframe, viewers)
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn owner_bots_watch_cameras_screens_audio_and_renew_revoke_viewers() {
    let db = database();
    let (_, owner) = user(&db, "video_bot_owner");
    let (_, member) = user(&db, "video_bot_member");
    permissions::bootstrap_owner(&db.pool, "video_bot_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Bot video"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":"video_bot_member"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"video","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let defaults = serde_json::to_value(Settings::default()).unwrap();
    let good = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":defaults}),
        StatusCode::OK,
    )
    .await["bots"][0]["id"]
        .clone();
    let mut weak = defaults.clone();
    weak["profile"] = json!("low_bandwidth");
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":weak}),
        StatusCode::OK,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut publisher = Peer::new(join(addr, &member, guild.clone(), channel.clone()).await).await;
    let mut lossy = defaults.clone();
    lossy["profile"] = json!("high_loss");
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":lossy}),
        StatusCode::OK,
    )
    .await;
    roster(&mut publisher, 3).await;
    send(
        &mut publisher.socket,
        ClientEvent::Screen {
            active: true,
            audio: true,
            quality: None,
        },
    )
    .await;
    send(&mut publisher.socket, ClientEvent::Camera { active: true }).await;
    let frame = video_frame();
    let (keyframe, viewers) = pump_video(&mut publisher, &frame, Duration::from_secs(5)).await;
    assert!(
        keyframe,
        "bots request initial video recovery over signaling"
    );
    assert_eq!(
        viewers, 3,
        "all bots must appear as screen and camera viewers"
    );
    let listed = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    for bot in listed["bots"].as_array().unwrap() {
        assert!(
            bot["received_system_audio_packets"].as_u64().unwrap() > 0,
            "{bot}"
        );
        let streams = bot["video"].as_array().unwrap();
        assert_eq!(streams.len(), 2, "{bot}");
        for stream in streams {
            assert!(stream["decoded_frames"].as_u64().unwrap() > 0, "{stream}");
            assert_eq!(stream["width"], 320);
            assert_eq!(stream["height"], 180);
        }
    }
    let before = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == good)
        .unwrap()["video"][0]["decoded_frames"]
        .as_u64()
        .unwrap();
    // Receiving continues well beyond the three-second view lease.
    pump_video(&mut publisher, &frame, Duration::from_secs(4)).await;
    let listed = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    let good_bot = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == good)
        .unwrap();
    assert!(good_bot["video"][0]["decoded_frames"].as_u64().unwrap() > before);
    let lossy_bot = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["settings"]["profile"] == "high_loss")
        .unwrap();
    assert!(
        lossy_bot["video"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["dropped_packets"].as_u64().unwrap() > 0)
    );
    assert!(
        lossy_bot["video"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["bitrate"].as_u64().unwrap() < 16_000_000),
        "loss feedback must reduce viewer demand: {lossy_bot}"
    );
    let old_epoch = good_bot["video"][0]["epoch"].clone();
    let mut deaf = defaults.clone();
    deaf["audio"]["deafened"] = json!(true);
    let connection = good_bot["connection_id"].clone();
    save_audio(&app, &owner, &connection, deaf["audio"].clone()).await;
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"update","bot_id":good,"revision":0,"settings":deaf}),
        StatusCode::OK,
    )
    .await;
    pump_video(&mut publisher, &frame, Duration::from_secs(1)).await;
    let listed = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    let good_bot = listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == good)
        .unwrap();
    assert!(good_bot["video"].as_array().unwrap().is_empty());
    let audio_before = good_bot["received_system_audio_packets"].clone();
    pump_video(&mut publisher, &frame, Duration::from_secs(1)).await;
    let listed = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        listed["bots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == good)
            .unwrap()["received_system_audio_packets"],
        audio_before
    );
    save_audio(&app, &owner, &connection, defaults["audio"].clone()).await;
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"update","bot_id":good,"revision":1,"settings":defaults}),
        StatusCode::OK,
    )
    .await;
    send(
        &mut publisher.socket,
        ClientEvent::Screen {
            active: false,
            audio: false,
            quality: None,
        },
    )
    .await;
    send(&mut publisher.socket, ClientEvent::Camera { active: false }).await;
    pump_video(&mut publisher, &frame, Duration::from_secs(1)).await;
    wait_for_no_video(&app, &owner, &guild, &channel).await;
    send(
        &mut publisher.socket,
        ClientEvent::Screen {
            active: true,
            audio: true,
            quality: None,
        },
    )
    .await;
    send(&mut publisher.socket, ClientEvent::Camera { active: true }).await;
    pump_video(&mut publisher, &frame, Duration::from_secs(3)).await;
    let listed = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"list"}),
        StatusCode::OK,
    )
    .await;
    let stream = &listed["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == good)
        .unwrap()["video"][0];
    assert_ne!(stream["epoch"], old_epoch);
    assert!(stream["decoded_frames"].as_u64().unwrap() > 0);
    let (status, _) = call(&app, ACCOUNT_PATH, &owner, json!({"action":"logout"})).await;
    assert_eq!(status, StatusCode::OK);
    let members = roster(&mut publisher, 0).await;
    assert!(
        members
            .iter()
            .all(|m| m.screen_viewers.is_empty() && m.camera_viewers.is_empty())
    );
    publisher.close().await;
    serving.abort();
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
    let status = audio(
        &app,
        &owner,
        &first["connection_id"],
        json!({"command":"audio_status"}),
    )
    .await;
    assert!(status["running"].as_bool().unwrap());
    assert!(!status["streams"].as_array().unwrap().is_empty());
    let _ = listed;
    let (status, _) = call(
        &app,
        thiscord_shared::remote_audio::PATH,
        &admin,
        json!({"connection_id":first["connection_id"],"command":{"command":"audio_current"}}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "instance admins cannot control test bots"
    );
    let mut muted = defaults.clone();
    muted["audio"]["muted"] = json!(true);
    save_audio(
        &app,
        &owner,
        &first["connection_id"],
        muted["audio"].clone(),
    )
    .await;
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
    bad["audio"]["input_gain"] = json!(99);
    bots(
        &app,
        &owner,
        &guild,
        &voice,
        json!({"action":"update","bot_id":first["id"],"revision":1,"settings":bad}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    save_audio(
        &app,
        &owner,
        &first["connection_id"],
        defaults["audio"].clone(),
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let empty = bots(
            &app,
            &admin,
            &guild,
            &voice,
            json!({"action":"list"}),
            StatusCode::OK,
        )
        .await;
        if empty["bots"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "revoked companions were not reaped"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL and the voice-bot companion"]
async fn owner_bots_use_native_recovery_dsp_devices_and_ptt() {
    let db = database();
    let (_, owner) = user(&db, "native_bot_owner");
    permissions::bootstrap_owner(&db.pool, "native_bot_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Native bot audio"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"audio","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let mut settings = serde_json::to_value(Settings::default()).unwrap();
    settings["profile"] = json!("high_loss");
    settings["talking"] = json!(true);
    settings["audio"]["mode"] = json!("push_to_talk");
    settings["audio"]["packet_redundancy"] = json!(true);
    settings["audio"]["adaptive_fec"] = json!(true);
    let source = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":settings}),
        StatusCode::OK,
    )
    .await["bots"][0]
        .clone();
    let mut silent = serde_json::to_value(Settings::default()).unwrap();
    silent["scenario"] = json!("silence");
    let list = bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"add","settings":silent}),
        StatusCode::OK,
    )
    .await;
    let receiver = list["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] != source["id"])
        .unwrap()
        .clone();
    async fn recovered(app: &Router, owner: &str, connection: &Value, field: &str) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let status = audio(app, owner, connection, json!({"command":"audio_status"})).await;
            if status["quality"]["streams"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s[field].as_u64().unwrap_or(0) > 0)
            {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "No {field} from native receiver: {status}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let status = recovered(
        &app,
        &owner,
        &receiver["connection_id"],
        "red_recovered_frames",
    )
    .await;
    let stream = &status["streams"][0];
    audio(&app, &owner, &receiver["connection_id"], json!({"command":"audio_volume","stream":stream["id"].as_str().unwrap().parse::<usize>().unwrap(),"target":stream["target"],"gain":0.25})).await;
    let devices = audio(
        &app,
        &owner,
        &receiver["connection_id"],
        json!({"command":"audio_devices"}),
    )
    .await;
    assert_eq!(devices.as_array().unwrap().len(), 2);
    // Both endpoints use the production DRED encoder/decoder; no RED fallback here.
    settings["audio"]["packet_redundancy"] = json!(false);
    settings["audio"]["deep_redundancy"] = json!(true);
    silent["audio"]["deep_redundancy"] = json!(true);
    save_audio(
        &app,
        &owner,
        &receiver["connection_id"],
        silent["audio"].clone(),
    )
    .await;
    save_audio(
        &app,
        &owner,
        &source["connection_id"],
        settings["audio"].clone(),
    )
    .await;
    recovered(
        &app,
        &owner,
        &receiver["connection_id"],
        "dred_output_frames",
    )
    .await;
    let fec = audio(
        &app,
        &owner,
        &source["connection_id"],
        json!({"command":"audio_status"}),
    )
    .await;
    assert_eq!(fec["quality"]["adaptive_fec"]["enabled"], true);
    // Exercise a real device swap, neural DSP preparation and normal settings round trip.
    let mut processing = silent["audio"].clone();
    processing["input"] = json!("virtual-microphone");
    processing["output"] = json!("virtual-speakers");
    processing["noise_suppression"] = json!(true);
    processing["noise_suppression_model"] = json!("deep_filter_net3");
    processing["automatic_gain"] = json!(true);
    processing["echo_cancellation"] = json!(true);
    processing["neural_echo"] = json!(true);
    processing["input_gain"] = json!(2.0);
    processing["output_volume"] = json!(0.5);
    processing["activation_threshold"] = json!(0.02);
    save_audio(&app, &owner, &receiver["connection_id"], processing.clone()).await;
    let observed: thiscord_shared::audio::AudioSettings = serde_json::from_value(
        audio(
            &app,
            &owner,
            &receiver["connection_id"],
            json!({"command":"audio_current"}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        observed,
        serde_json::from_value(processing.clone()).unwrap()
    );
    let status = audio(
        &app,
        &owner,
        &receiver["connection_id"],
        json!({"command":"audio_status"}),
    )
    .await;
    assert_eq!(status["running"], true);
    assert_eq!(status["echo"]["automatic_gain"], true);
    // Live switches to the other prewarmed denoisers, with non-default strengths.
    for (model, strength) in [("rnnoise", "light"), ("gtcrn", "balanced")] {
        processing["noise_suppression_model"] = json!(model);
        processing["noise_suppression_strength"] = json!(strength);
        save_audio(&app, &owner, &receiver["connection_id"], processing.clone()).await;
        let observed: thiscord_shared::audio::AudioSettings = serde_json::from_value(
            audio(
                &app,
                &owner,
                &receiver["connection_id"],
                json!({"command":"audio_current"}),
            )
            .await,
        )
        .unwrap();
        assert_eq!(
            observed,
            serde_json::from_value(processing.clone()).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        let status = audio(
            &app,
            &owner,
            &receiver["connection_id"],
            json!({"command":"audio_status"}),
        )
        .await;
        // Inference failures stop the engine rather than passing raw audio.
        assert_eq!(status["running"], true, "{model}: {}", status["message"]);
    }
    settings["talking"] = json!(false);
    bots(
        &app,
        &owner,
        &guild,
        &channel,
        json!({"action":"update","bot_id":source["id"],"revision":0,"settings":settings}),
        StatusCode::OK,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let stopped = audio(
        &app,
        &owner,
        &source["connection_id"],
        json!({"command":"audio_status"}),
    )
    .await;
    assert_eq!(stopped["transmitting"], false);
    for bot in [&source, &receiver] {
        bots(
            &app,
            &owner,
            &guild,
            &channel,
            json!({"action":"remove","bot_id":bot["id"]}),
            StatusCode::OK,
        )
        .await;
    }
}
