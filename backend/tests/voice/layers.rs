use super::*;
use thiscord_shared::{screen::*, voice::MediaKind};

async fn publish(
    source: &Peer,
    layer: usize,
    sequence: u16,
    timestamp: u32,
    epoch: u32,
    key: bool,
) {
    // STAP-A SPS/PPS/IDR is the recovery boundary; distinct bytes prove routing.
    let payload = if key {
        vec![
            24,
            0,
            2,
            0x67,
            layer as u8,
            0,
            2,
            0x68,
            layer as u8,
            0,
            2,
            0x65,
            layer as u8,
        ]
    } else {
        vec![0x41, layer as u8]
    };
    let track = if layer == 0 {
        &source.screen_tracks[0]
    } else {
        source.low_track.as_ref().unwrap()
    };
    track
        .write_rtp(rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type: 125,
                ssrc: if layer == 0 { 901 } else { LOW_LAYER_SSRC },
                sequence_number: sequence,
                timestamp,
                csrc: LayerPacket {
                    epoch,
                    bitrate: if layer == 0 { 5_000_000 } else { 166_666 },
                }
                .csrc(),
                marker: true,
                ..Default::default()
            },
            payload: payload.into(),
        })
        .await
        .unwrap();
}
async fn video(peer: &mut Peer, source: usize) -> rtp::Packet {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let packet = peer.packets.recv().await.unwrap();
            if packet.header.ssrc == MediaKind::ScreenVideo.relay_ssrc(source) {
                break packet;
            }
        }
    })
    .await
    .expect("selected screen layer did not arrive")
}
async fn views(peer: &mut Peer, bitrate: u32) {
    peer.views[0].bitrate = bitrate;
    send(
        &mut peer.socket,
        ClientEvent::ScreenViews {
            views: peer.views.clone(),
        },
    )
    .await;
    still_connected(&mut peer.socket).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn encrypted_layers_isolate_fast_slow_and_legacy_viewers_and_route_recovery() {
    let db = database();
    let (_, owner_token) = user(&db, "layers_owner");
    let (_, fast_token) = user(&db, "layers_fast");
    let (_, slow_token) = user(&db, "layers_slow");
    permissions::bootstrap_owner(&db.pool, "layers_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner_token,
        json!({"action":"create_guild","name":"Layers"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for username in ["layers_fast", "layers_slow"] {
        change(
            &app,
            &owner_token,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &owner_token,
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
    let mut source = Peer::with_layers(
        join(addr, &owner_token, guild.clone(), channel.clone()).await,
        64,
        false,
        true,
    )
    .await;
    // Viewers intentionally use the old negotiation: no receiver-side extension is necessary.
    let mut fast = Peer::new(join(addr, &fast_token, guild.clone(), channel.clone()).await).await;
    let mut slow = Peer::new(join(addr, &slow_token, guild, channel).await).await;
    forwarded(&mut source, &mut fast).await;
    forwarded(&mut source, &mut slow).await;
    send(
        &mut source.socket,
        ClientEvent::Screen {
            active: true,
            audio: true,
            quality: Some(Quality::default()),
        },
    )
    .await;
    watch_screen(&mut source, &mut fast).await;
    watch_screen(&mut source, &mut slow).await;
    views(&mut fast, 5_000_000).await;
    views(&mut slow, MIN_VIEW_BITRATE).await;
    let epoch = fast.views[0].epoch;
    let plan = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerEvent::ScreenTarget {
                layers: Some(plan), ..
            } = event(&mut source.socket).await
                && plan.low > 0
            {
                break plan;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        plan,
        LayerTargets {
            primary: 5_000_000,
            low: MIN_VIEW_BITRATE
        }
    );
    publish(&source, 0, 100, 9000, epoch, true).await;
    publish(&source, 1, 500, 9000, epoch, true).await;
    assert_eq!(video(&mut fast, source.slot).await.payload.last(), Some(&0));
    let first = video(&mut slow, source.slot).await;
    assert_eq!(first.payload.last(), Some(&1));
    // A weak viewer's loss requests the low encoder, not the fast viewer's encoder.
    send(
        &mut slow.socket,
        ClientEvent::ScreenKeyframe {
            kind: MediaKind::ScreenVideo,
            slot: source.slot,
            epoch,
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerEvent::ScreenKeyframe {
                layer,
                epoch: requested,
                ..
            } = event(&mut source.socket).await
            {
                assert_eq!(layer, Some(1));
                assert_eq!(requested, epoch);
                break;
            }
        }
    })
    .await
    .unwrap();
    views(&mut slow, 5_000_000).await;
    views(&mut fast, 5_000_000).await;
    publish(&source, 0, 101, 12000, epoch, false).await;
    assert_eq!(
        video(&mut fast, source.slot).await.payload.as_ref(),
        [0x41, 0]
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), slow.packets.recv())
            .await
            .is_err()
    );
    publish(&source, 0, 102, 15000, epoch, true).await;
    video(&mut fast, source.slot).await;
    let upgraded = video(&mut slow, source.slot).await;
    assert_eq!(upgraded.payload.last(), Some(&0));
    assert_eq!(
        upgraded.header.sequence_number,
        first.header.sequence_number.wrapping_add(1)
    );
    assert!(
        upgraded
            .header
            .timestamp
            .wrapping_sub(first.header.timestamp) as i32
            > 0
    );
    // The next subscription boundary cannot reuse SRTP sequence numbers.
    send(&mut slow.socket, ClientEvent::ScreenViews { views: vec![] }).await;
    still_connected(&mut slow.socket).await;
    views(&mut slow, MIN_VIEW_BITRATE).await;
    publish(&source, 1, 501, 18000, epoch, true).await;
    let resumed = video(&mut slow, source.slot).await;
    assert_eq!(
        resumed.header.sequence_number,
        upgraded.header.sequence_number.wrapping_add(1)
    );
    // Wrong source epoch and expired leases must not admit either layer.
    publish(&source, 1, 502, 21000, epoch.wrapping_sub(1), true).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(80), slow.packets.recv())
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_secs(VIEW_LEASE_SECS)).await;
    publish(&source, 0, 103, 24000, epoch, true).await;
    publish(&source, 1, 503, 24000, epoch, true).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(80), slow.packets.recv())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), fast.packets.recv())
            .await
            .is_err()
    );
    // After the fast lease expires, the SFU asks the primary to downscale. Until
    // that reconfiguration occurs, the old high-rate primary must not leak to
    // the remaining low-budget viewer just because its selected index changed.
    views(&mut slow, MIN_VIEW_BITRATE).await;
    publish(&source, 0, 104, 27000, epoch, true).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(80), slow.packets.recv())
            .await
            .is_err()
    );
    source.close().await;
    fast.close().await;
    slow.close().await;
    serving.abort();
}
