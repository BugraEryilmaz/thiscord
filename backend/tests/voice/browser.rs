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

    // A browser picks a random SSRC and also offers an (ignored) video track.
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
