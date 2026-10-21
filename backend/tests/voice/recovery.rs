//! Real production SFU with authenticated SRTCP and two encrypted RTP hops.
use super::*;
use thiscord_shared::{screen::*, voice::MediaKind};

struct Call {
    _db: Database,
    app: Router,
    state: Value,
    owner_token: String,
    viewer_id: AccountId,
    source: Peer,
    viewer: Peer,
    serving: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Call {
    async fn new(layered: bool) -> Self {
        let db = database();
        let (_, owner_token) = user(&db, "recovery_owner");
        let (viewer_id, viewer_token) = user(&db, "recovery_viewer");
        permissions::bootstrap_owner(&db.pool, "recovery_owner").unwrap();
        let app = api::router(
            Some(db.pool.clone()),
            vec!["http://localhost:1420".parse().unwrap()],
        );
        let mut state = command(
            &app,
            &owner_token,
            json!({"action":"create_guild","name":"Recovery"}),
            StatusCode::OK,
        )
        .await["state"]
            .clone();
        change(
            &app,
            &owner_token,
            &mut state,
            json!({"action":"add_member","username":"recovery_viewer"}),
            StatusCode::OK,
        )
        .await;
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
        let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
        let mut source = Peer::with_layers(
            join(addr, &owner_token, guild.clone(), channel.clone()).await,
            64,
            false,
            layered,
        )
        .await;
        let mut viewer = Peer::new(join(addr, &viewer_token, guild, channel).await).await;
        forwarded(&mut source, &mut viewer).await;
        send(
            &mut source.socket,
            ClientEvent::Screen {
                active: true,
                audio: true,
                quality: layered.then(Quality::default),
            },
        )
        .await;
        watch_screen(&mut source, &mut viewer).await;
        Self {
            _db: db,
            app,
            state,
            owner_token,
            viewer_id,
            source,
            viewer,
            serving,
        }
    }
    async fn views(&mut self, enabled: bool) {
        send(
            &mut self.viewer.socket,
            ClientEvent::ScreenViews {
                views: if enabled {
                    self.viewer.views.clone()
                } else {
                    vec![]
                },
            },
        )
        .await;
        still_connected(&mut self.viewer.socket).await;
    }
    async fn close(self) {
        self.source.close().await;
        self.viewer.close().await;
        self.serving.abort();
    }
    async fn packet(&self, seq: u16, timestamp: u32, payload: Vec<u8>) {
        self.source.screen_tracks[0]
            .write_rtp(rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: 125,
                    ssrc: 901,
                    sequence_number: seq,
                    timestamp,
                    marker: true,
                    csrc: LayerPacket {
                        epoch: self.viewer.views[0].epoch,
                        bitrate: 5_000_000,
                    }
                    .csrc(),
                    ..Default::default()
                },
                payload: payload.into(),
            })
            .await
            .unwrap();
    }
    async fn video(&mut self) -> rtp::Packet {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let packet = self.viewer.packets.recv().await.unwrap();
                if packet.header.ssrc == MediaKind::ScreenVideo.relay_ssrc(self.source.slot) {
                    break packet;
                }
            }
        })
        .await
        .expect("fresh encrypted screen recovery must arrive")
    }
    async fn silent(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), self.viewer.packets.recv())
                .await
                .is_err()
        );
    }
}
fn idr(tag: u8) -> Vec<u8> {
    vec![24, 0, 2, 0x67, tag, 0, 2, 0x68, tag, 0, 2, 0x65, tag]
}
async fn nack(peer: &Peer, packet: &rtp::Packet, expanded: bool) {
    use rtc::rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack};
    peer.track
        .write_rtcp(vec![Box::new(TransportLayerNack {
            sender_ssrc: 900,
            media_ssrc: packet.header.ssrc,
            nacks: vec![
                NackPair {
                    packet_id: packet.header.sequence_number,
                    lost_packets: if expanded { u16::MAX } else { 0 }
                };
                if expanded { 32 } else { 1 }
            ],
        })])
        .await
        .unwrap();
}
async fn wire_bytes(peer: &Peer) -> u64 {
    let report = peer
        .pc
        .get_stats(
            std::time::Instant::now(),
            rtc::statistics::StatsSelector::None,
        )
        .await;
    report
        .iter()
        .filter_map(|e| match e {
            rtc::statistics::report::RTCStatsReportEntry::IceCandidatePair(s) if s.nominated => {
                Some(s.bytes_received)
            }
            _ => None,
        })
        .sum()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn encrypted_screen_nacks_cannot_amplify_or_bypass_lifecycle_and_camera_still_repairs() {
    let mut call = Call::new(true).await;
    call.viewer.views[0].bitrate = MIN_VIEW_BITRATE;
    call.views(true).await;
    call.packet(1, 9000, idr(1)).await;
    call.video().await;
    call.packet(2, 12000, vec![0x41; 1200]).await;
    let cached = call.video().await;

    // Measure ICE wire bytes: SRTP replay protection would hide duplicate media
    // from the application while the vulnerable responder still floods the wire.
    for phase in [
        "live",
        "unsubscribe",
        "lease",
        "stop",
        "new_epoch",
        "revocation",
    ] {
        match phase {
            "unsubscribe" => call.views(false).await,
            "lease" => {
                call.views(true).await;
                tokio::time::sleep(
                    Duration::from_secs(VIEW_LEASE_SECS) + Duration::from_millis(50),
                )
                .await;
            }
            "stop" => {
                call.views(true).await;
                send(
                    &mut call.source.socket,
                    ClientEvent::Screen {
                        active: false,
                        audio: false,
                        quality: None,
                    },
                )
                .await;
                still_connected(&mut call.source.socket).await;
            }
            "new_epoch" => {
                send(
                    &mut call.source.socket,
                    ClientEvent::Screen {
                        active: true,
                        audio: true,
                        quality: Some(Quality::default()),
                    },
                )
                .await;
                watch_screen(&mut call.source, &mut call.viewer).await;
                call.viewer.views[0].bitrate = MIN_VIEW_BITRATE;
                call.views(true).await;
                call.packet(3, 15000, idr(3)).await;
                assert_eq!(call.video().await.payload.as_ref(), idr(3));
            }
            "revocation" => {
                let channel = call.state["channels"][0]["id"].clone();
                change(&call.app, &call.owner_token, &mut call.state,
                    json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":call.viewer_id},"allow":[],"deny":["join_voice"]}), StatusCode::OK).await;
            }
            _ => {}
        }
        let before = wire_bytes(&call.viewer).await;
        let started = std::time::Instant::now();
        for n in 0..100 {
            nack(&call.viewer, &cached, n % 2 == 0).await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let received = wire_bytes(&call.viewer).await - before;
        let envelope = 7500.0 + 30_000.0 * started.elapsed().as_secs_f64();
        println!("NACK {phase}: {received} wire bytes, screen envelope {envelope:.0}");
        assert!(
            (received as f64) < envelope,
            "NACK bypassed view budget: {phase}"
        );
        // Only periodic RTCP/ICE is allowed. 100 cached RTP copies exceed 120KB.
        assert!(
            received < 4000,
            "cached screen bytes escaped lifecycle gate: {phase}"
        );
    }
    call.close().await;

    // Positive control on the SAME production responder: camera uses the same
    // H.264 codec yet must retain NACK recovery. Also exercise microphone/audio.
    let mut call = Call::new(false).await;
    send(
        &mut call.source.socket,
        ClientEvent::Camera { active: true },
    )
    .await;
    let source = camera_roster(&mut call.source).await;
    call.viewer.views.push(Subscription {
        kind: MediaKind::Camera,
        slot: source.slot,
        owner: source.account_id,
        epoch: source.camera_epoch,
        bitrate: 1_500_000,
    });
    screen_forwarded(&mut call.source, &mut call.viewer, 2).await;
    call.source.screen_sequences[2] += 1;
    call.source.screen_tracks[2]
        .write_rtp(rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type: 125,
                ssrc: 903,
                sequence_number: call.source.screen_sequences[2],
                timestamp: u32::from(call.source.screen_sequences[2]) * 9000,
                marker: true,
                ..Default::default()
            },
            payload: vec![0x65; 1200].into(),
        })
        .await
        .unwrap();
    let packet = tokio::time::timeout(Duration::from_secs(2), call.viewer.packets.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        packet.header.ssrc,
        MediaKind::Camera.relay_ssrc(source.slot)
    );
    let before = wire_bytes(&call.viewer).await;
    for _ in 0..30 {
        nack(&call.viewer, &packet, false).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        wire_bytes(&call.viewer).await - before >= 30 * 1200,
        "camera NACK responder disabled"
    );
    forwarded(&mut call.source, &mut call.viewer).await;
    screen_forwarded(&mut call.source, &mut call.viewer, 1).await;
    call.close().await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn encrypted_large_screen_gaps_recover_at_idr_for_legacy_layers_and_wrap() {
    for layered in [false, true] {
        let mut call = Call::new(layered).await;
        call.packet(1, 9000, idr(1)).await;
        let first = call.video().await;
        let mut last = first.header.sequence_number;
        let mut seen = std::collections::HashSet::from([last]);
        // The exact 4096-lost-packets report, then larger gaps and a wire wrap.
        for (n, seq) in [4098_u16, 25000, 50000, 65000, 5000]
            .into_iter()
            .enumerate()
        {
            call.views(true).await;
            let ts = 18000 + n as u32 * 9000;
            call.packet(seq, ts, vec![0x41, 1]).await;
            call.silent().await; // arbitrary forward deltas cannot resynchronize
            // A fresh bare IDR can use legacy cached parameters; layered streams
            // must validate the contiguous SPS/PPS/IDR prefix.
            let payload = if layered {
                idr(n as u8 + 2)
            } else {
                vec![0x65, n as u8 + 2]
            };
            call.packet(seq + 1, ts + 3000, payload.clone()).await;
            let recovered = call.video().await;
            assert_eq!(recovered.payload.as_ref(), payload);
            assert_eq!(recovered.header.sequence_number, last.wrapping_add(1));
            assert!(seen.insert(recovered.header.sequence_number));
            last = recovered.header.sequence_number;
            // Duplicate IDRs and delayed packets must not reset the mapping.
            call.packet(seq + 1, ts + 3000, payload).await;
            call.packet(seq, ts, idr(99)).await;
            call.silent().await;
            call.packet(seq + 3, ts + 6000, vec![0x41, 3]).await;
            let ahead = call.video().await;
            call.packet(seq + 2, ts + 3000, vec![0x41, 2]).await;
            let reordered = call.video().await;
            assert_eq!(ahead.header.sequence_number, last.wrapping_add(2));
            assert_eq!(reordered.header.sequence_number, last.wrapping_add(1));
            assert!(seen.insert(ahead.header.sequence_number));
            assert!(seen.insert(reordered.header.sequence_number));
            last = ahead.header.sequence_number;
        }
        call.views(false).await;
        call.views(true).await;
        call.packet(10000, 90000, idr(7)).await;
        assert_eq!(
            call.video().await.header.sequence_number,
            last.wrapping_add(1)
        );
        forwarded(&mut call.source, &mut call.viewer).await;
        call.close().await;
    }
}
