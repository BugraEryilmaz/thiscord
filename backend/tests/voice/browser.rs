//! Browser microphone publishers: random SSRC, plain Opus, cookie-authenticated
//! signaling. Desktop publishers keep the fixed-SSRC contract.
use super::*;

/// Browsers send the session only as the HttpOnly cookie; the Join token is empty.
async fn join_with_cookie(
    addr: std::net::SocketAddr,
    cookie: &str,
    guild: Value,
    channel: Value,
) -> Socket {
    let mut req = format!("ws://{addr}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    req.headers_mut().insert(
        "cookie",
        format!("__Host-thiscord_session={cookie}").parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    send(
        &mut socket,
        ClientEvent::Join {
            token: String::new(),
            guild_id: serde_json::from_value(guild).unwrap(),
            channel_id: serde_json::from_value(channel).unwrap(),
            attempt: None,
        },
    )
    .await;
    socket
}

async fn none_forwarded(source: &mut Peer, receiver: &mut Peer) {
    while receiver.packets.try_recv().is_ok() {}
    for _ in 0..40 {
        source.publish().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), receiver.packets.recv())
            .await
            .is_err(),
        "unflagged random-SSRC publisher must not be forwarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn browser_publishers_forward_plain_opus_from_any_ssrc() {
    let db = database();
    let (_, owner) = user(&db, "browser_owner");
    let (_, legacy_token) = user(&db, "browser_legacy");
    let (_, modern_token) = user(&db, "browser_modern");
    let (_, impostor) = user(&db, "browser_impostor");
    permissions::bootstrap_owner(&db.pool, "browser_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Browser"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for username in ["browser_legacy", "browser_modern", "browser_impostor"] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let guild = state["guild"]["id"].clone();
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app).into_future());

    // A browser picks random SSRCs and also sends idle screen/camera senders.
    let browser_ssrc = 0x5eed_0000 | (rand_u16() as u32);
    let mut browser = Peer::with_publisher(
        join_with_cookie(addr, &owner, guild.clone(), channel.clone()).await,
        64,
        false,
        false,
        false,
        browser_ssrc,
        true,
    )
    .await;
    let mut legacy =
        Peer::new(join(addr, &legacy_token, guild.clone(), channel.clone()).await).await;
    let mut modern = Peer::with_red(
        join(addr, &modern_token, guild.clone(), channel.clone()).await,
        64,
        true,
    )
    .await;
    for _ in 0..5 {
        browser.publish_screen(0).await;
    }
    let relay_ssrc = voice::MediaKind::Microphone.relay_ssrc(browser.slot);
    let packet = forwarded(&mut browser, &mut legacy).await;
    assert_eq!(packet.header.ssrc, relay_ssrc);
    assert_eq!(packet.header.payload_type, 111, "plain Opus to non-RED");
    assert_eq!(&packet.payload[..], &[0xf8, 0xff, 0xfe]);
    let packet = forwarded(&mut browser, &mut modern).await;
    assert_eq!(packet.header.ssrc, relay_ssrc);
    // Desktop publishers still reach the browser's receive path.
    let packet = forwarded(&mut legacy, &mut browser).await;
    assert_eq!(
        packet.header.ssrc,
        voice::MediaKind::Microphone.relay_ssrc(legacy.slot)
    );
    still_connected(&mut browser.socket).await;

    // Without the flag, an unknown SSRC is still rejected.
    let mut unflagged = Peer::with_publisher(
        join(addr, &impostor, guild, channel).await,
        64,
        false,
        false,
        false,
        browser_ssrc.wrapping_add(1),
        false,
    )
    .await;
    none_forwarded(&mut unflagged, &mut legacy).await;
    // The browser connection is unaffected by the rejected peer.
    forwarded(&mut browser, &mut legacy).await;

    unflagged.close().await;
    browser.close().await;
    legacy.close().await;
    modern.close().await;
    serving.abort();
}

fn rand_u16() -> u16 {
    Uuid::new_v4().as_u128() as u16
}

/// RTCP PLI the browser stand-in received for one of its SSRCs.
fn pli_count(peer: &Peer, ssrc: u32) -> u32 {
    peer.keyframe_requests
        .lock()
        .unwrap()
        .get(&ssrc)
        .copied()
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn browser_publishers_share_screen_camera_and_audio_with_pli_keyframes() {
    use voice::MediaKind;
    let db = database();
    let (_, owner) = user(&db, "browser_video_owner");
    let (_, viewer_token) = user(&db, "browser_video_viewer");
    permissions::bootstrap_owner(&db.pool, "browser_video_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Browser video"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":"browser_video_viewer"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let guild = state["guild"]["id"].clone();
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app).into_future());

    let mut browser = Peer::with_publisher(
        join_with_cookie(addr, &owner, guild.clone(), channel.clone()).await,
        64,
        false,
        false,
        false,
        0x6ee0_0000 | (rand_u16() as u32),
        true,
    )
    .await;
    // A desktop viewer: fixed-SSRC peer, ordinary subscriptions.
    let mut viewer = Peer::new(join(addr, &viewer_token, guild, channel).await).await;
    assert!(
        browser
            .screen_ssrcs
            .iter()
            .all(|ssrc| !(900..=904).contains(ssrc))
    );

    send(&mut browser.socket, ClientEvent::Camera { active: true }).await;
    send(
        &mut browser.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: true,
        },
    )
    .await;
    let member = camera_roster(&mut browser).await;
    viewer.views = [MediaKind::ScreenVideo, MediaKind::Camera]
        .map(|kind| thiscord_shared::screen::Subscription {
            kind,
            slot: member.slot,
            owner: member.account_id,
            epoch: member.video_epoch(kind),
            bitrate: 1_500_000,
            h264_high: false,
        })
        .to_vec();
    // Screen video, shared audio and camera all reach the subscribed viewer on
    // the publisher slot's relay SSRCs, whatever SSRCs the browser chose.
    for kind in 0..3 {
        let packet = screen_forwarded(&mut browser, &mut viewer, kind).await;
        assert_eq!(
            packet.header.payload_type,
            if kind == 1 { 111 } else { 125 }
        );
        assert_eq!(
            &packet.payload[..],
            if kind == 1 {
                &[0xf8, 0xff, 0xfe][..]
            } else {
                &[0x65, 1, 2, 3][..]
            }
        );
        assert_eq!(
            packet.header.csrc,
            vec![if kind == 2 {
                member.camera_epoch
            } else {
                member.screen_epoch
            }]
        );
    }

    // Viewer keyframe demand reaches the browser as RTCP PLI on its own video
    // SSRCs; it never receives the native ScreenKeyframe event (which would
    // fail `still_connected`).
    for (kind, index) in [(MediaKind::ScreenVideo, 0), (MediaKind::Camera, 2)] {
        let ssrc = browser.screen_ssrcs[index];
        let before = pli_count(&browser, ssrc);
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                send(
                    &mut viewer.socket,
                    ClientEvent::ScreenKeyframe {
                        kind,
                        slot: member.slot,
                        epoch: member.video_epoch(kind),
                    },
                )
                .await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                if pli_count(&browser, ssrc) > before {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("keyframe demand for {kind:?} must reach the browser as PLI"));
    }
    still_connected(&mut browser.socket).await;
    still_connected(&mut viewer.socket).await;

    // Media sent on another member's m-lines is never forwarded and does not
    // end the browser's call.
    tokio::time::sleep(Duration::from_millis(150)).await;
    while viewer.packets.try_recv().is_ok() {}
    for round in 0..5_u16 {
        for (position, track) in browser.foreign_tracks.iter().enumerate() {
            let video = position >= voice::ROOM_CAPACITY - 1;
            let index = if video {
                voice::ROOM_CAPACITY + position - (voice::ROOM_CAPACITY - 1) + 1
            } else {
                position + 1
            };
            track
                .write_rtp(rtp::Packet {
                    header: rtp::header::Header {
                        version: 2,
                        payload_type: if video { 125 } else { 111 },
                        ssrc: browser.microphone_ssrc.wrapping_add(0x10 + index as u32),
                        sequence_number: round + 1,
                        timestamp: u32::from(round) * 960,
                        marker: true,
                        ..Default::default()
                    },
                    payload: if video {
                        vec![0x65, 9, 9, 9]
                    } else {
                        vec![0xf8, 0xff, 0xfe]
                    }
                    .into(),
                })
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(60), viewer.packets.recv())
                .await
                .is_err(),
            "media on another member's m-line must not be forwarded"
        );
    }
    still_connected(&mut browser.socket).await;

    // Stopping the share stops video and shared audio; the camera continues.
    send(
        &mut browser.socket,
        ClientEvent::Screen {
            quality: None,
            active: false,
            audio: false,
        },
    )
    .await;
    still_connected(&mut browser.socket).await;
    screen_blocked(&mut browser, &mut viewer).await;
    screen_forwarded(&mut browser, &mut viewer, 2).await;

    browser.close().await;
    viewer.close().await;
    serving.abort();
}
