use super::*;
use rtc::rtp;
use tokio::sync::Notify;
use webrtc::{
    media_stream::track_remote::{TrackRemote, TrackRemoteEvent},
    peer_connection::*,
};

#[cfg(target_os = "linux")]
#[path = "lifecycle_tests.rs"]
mod lifecycle;

#[test]
fn final_handle_drop_requests_shutdown() {
    let (commands, _) = mpsc::channel(1);
    let (cancel, cancelled) = watch::channel(false);
    let (_, status) = watch::channel(PlaybackEvent::Idle);
    let (_, closed) = watch::channel(false);
    let (_, publication) = watch::channel(Publication {
        generation: 0,
        restricted: false,
    });
    let voice = Voice(Arc::new(Inner {
        commands,
        publication,
        #[cfg(target_os = "linux")]
        hold_commands: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        cancel,
        status,
        closed,
    }));
    let clone = voice.clone();
    drop(voice);
    assert!(!*cancelled.borrow());
    drop(clone);
    assert!(*cancelled.borrow());
}

#[test]
fn roster_restriction_checks_current_own_slot() {
    assert!(own(&[], 0).is_err());
    let mut member: Participant = serde_json::from_value(serde_json::json!({
        "account_id":"00000000-0000-0000-0000-000000000001", "username":"bot",
        "slot":2,"muted":true,"deafened":true,"can_speak":true
    }))
    .unwrap();
    assert_eq!(member.publication_revision, 0);
    assert!(own(&[member.clone()], 2).is_ok());
    assert!(own(&[member.clone()], 1).is_err());
    member.can_speak = false;
    assert!(own(&[member], 2).is_err());
}

struct ReceiverHandler {
    gathered: Arc<Notify>,
    packets: mpsc::Sender<rtp::Packet>,
    stop: watch::Receiver<bool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for ReceiverHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let packets = self.packets.clone();
        let mut stop = self.stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! { biased;
                    _ = stop.changed() => break,
                    event = track.poll() => match event {
                        Some(TrackRemoteEvent::OnRtpPacket(packet)) => { let _ = packets.try_send(packet); },
                        None => break,
                        _ => {},
                    }
                }
            }
        });
    }
}
struct ReceiverPeer {
    pc: Arc<dyn PeerConnection>,
    gathered: Arc<Notify>,
    packets: mpsc::Receiver<rtp::Packet>,
    stop: watch::Sender<bool>,
}

#[tokio::test]
async fn unreachable_ice_server_does_not_block_direct_voice_answer() {
    // A bound socket that never replies models STUN/TURN being unreachable
    // from one of the bot's interfaces, including its loopback candidate.
    let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = blackhole.local_addr().unwrap();
    let mut bot = peer::Peer::new(&[thiscord_shared::voice::IceServer {
        urls: vec![
            format!("stun:{address}"),
            format!("turn:{address}?transport=udp"),
        ],
        username: "fixture".into(),
        credential: "fixture".into(),
    }])
    .await
    .unwrap();
    let receiver = ReceiverPeer::new().await;
    receiver
        .pc
        .add_track(peer::track() as Arc<dyn TrackLocal>)
        .await
        .unwrap();
    let offer = serde_json::to_string(&receiver.offer().await).unwrap();
    let result = timeout(Duration::from_secs(12), async {
        let answer = bot.answer(&offer).await?;
        let answer: RTCSessionDescription = serde_json::from_str(&answer).unwrap();
        assert!(answer.sdp.contains("a=candidate:"));
        receiver.pc.set_remote_description(answer).await.unwrap();
        bot.connected().await
    })
    .await;
    bot.close().await;
    receiver.close().await;
    result
        .expect("bot must answer before the SFU negotiation deadline")
        .unwrap();
}
impl ReceiverPeer {
    async fn new() -> Self {
        let gathered = Arc::new(Notify::new());
        let (packets, rx) = mpsc::channel(8);
        let (stop, cancellation) = watch::channel(false);
        let mut engine = MediaEngine::default();
        engine
            .register_codec(
                rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
                    rtp_codec: peer::codec(thiscord_shared::voice::MediaKind::ScreenVideo),
                    payload_type: thiscord_shared::voice::MediaKind::ScreenVideo.payload_type(),
                },
                rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video,
            )
            .unwrap();
        engine.register_default_codecs().unwrap();
        let pc = PeerConnectionBuilder::new()
            .with_media_engine(engine)
            .with_handler(Arc::new(ReceiverHandler {
                gathered: gathered.clone(),
                packets,
                stop: cancellation,
            }))
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .build()
            .await
            .unwrap();
        Self {
            pc: Arc::new(pc),
            gathered,
            packets: rx,
            stop,
        }
    }
    async fn offer(&self) -> RTCSessionDescription {
        self.pc
            .set_local_description(self.pc.create_offer(None).await.unwrap())
            .await
            .unwrap();
        self.gathered.notified().await;
        self.pc.local_description().await.unwrap()
    }
    async fn close(&self) {
        self.stop.send_replace(true);
        let _ = timeout(IO_TIMEOUT, self.pc.close()).await;
    }
}
impl Drop for ReceiverPeer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        let pc = self.pc.clone();
        tokio::spawn(async move {
            let _ = timeout(IO_TIMEOUT, pc.close()).await;
        });
    }
}

#[tokio::test]
async fn video_answer_negotiates_three_sendonly_tracks() {
    use thiscord_shared::voice::MediaKind;
    timeout(Duration::from_secs(10), async {
        let mut bot = peer::Peer::new(&[]).await.unwrap();
        let receiver = ReceiverPeer::new().await;
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            receiver
                .pc
                .add_track(peer::media_track(kind))
                .await
                .unwrap();
        }
        let offer = serde_json::to_string(&receiver.offer().await).unwrap();
        let answer = bot.answer_with_screen(&offer, true).await.unwrap();
        let answer: RTCSessionDescription = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer.sdp.matches("a=sendonly").count(), 3);
        assert!(!answer.sdp.contains("a=recvonly") && !answer.sdp.contains("a=sendrecv"));
        assert!(answer.sdp.contains("H264/90000"));
        receiver.pc.set_remote_description(answer).await.unwrap();
        bot.connected().await.unwrap();
        bot.close().await;
        receiver.close().await;
    })
    .await
    .unwrap();
}

/// Real Opus and two real encrypted WebRTC hops. The middle peer forwards the
/// encoded payload as the SFU does. No audio devices, ffmpeg or database needed.
#[tokio::test]
async fn outgoing_opus_survives_two_encrypted_hops_without_receivers() {
    let result = timeout(Duration::from_secs(20), async {
        let mut bot = peer::Peer::new(&[]).await.unwrap();
        let mut ingress = ReceiverPeer::new().await;
        let relay = ReceiverPeer::new().await;
        let mut listener = ReceiverPeer::new().await;
        // One server sendrecv audio section is sufficient: no video codecs or
        // receiver track roster is required by the bot implementation.
        ingress.pc.add_track(peer::track() as Arc<dyn TrackLocal>).await.unwrap();
        let offer = ingress.offer().await;
        let answer = bot.answer(&serde_json::to_string(&offer).unwrap()).await.unwrap();
        let answer: RTCSessionDescription = serde_json::from_str(&answer).unwrap();
        assert!(answer.sdp.contains("a=sendonly"));
        assert!(!answer.sdp.contains("a=sendrecv") && !answer.sdp.contains("a=recvonly"));
        ingress.pc.set_remote_description(answer).await.unwrap();
        bot.connected().await.unwrap();

        let forward = peer::track();
        relay.pc.add_track(forward.clone() as Arc<dyn TrackLocal>).await.unwrap();
        listener.pc.set_remote_description(relay.offer().await).await.unwrap();
        listener.pc.set_local_description(listener.pc.create_answer(None).await.unwrap()).await.unwrap();
        listener.gathered.notified().await;
        relay.pc.set_remote_description(listener.pc.local_description().await.unwrap()).await.unwrap();

        let raw: Vec<_> = (0..1920).flat_map(|i| (((i / 2) as f32 * 0.04).sin().mul_add(6000.0, 0.0) as i16).to_le_bytes()).collect();
        let (tx, mut frames) = mpsc::channel(1);
        media::encode(raw.as_slice(), tx).await.unwrap();
        let frame = frames.recv().await.unwrap();
        let mut clock = peer::RtpClock::new();
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                _ = tick.tick() => { bot.track.write_rtp(clock.packet(frame.clone(), false)).await.unwrap(); },
                Some(packet) = ingress.packets.recv() => {
                    assert_eq!(packet.payload, frame);
                    assert_eq!(packet.header.ssrc, thiscord_shared::voice::MediaKind::Microphone.publisher_ssrc());
                    forward.write_rtp(packet).await.unwrap();
                },
                Some(packet) = listener.packets.recv() => {
                    assert_eq!(packet.payload, frame);
                    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
                    let mut pcm = [0_i16; 1920];
                    assert_eq!(decoder.decode(&packet.payload, &mut pcm, false).unwrap(), 960);
                    assert!(pcm.iter().any(|sample| sample.abs() > 100));
                    break;
                },
            }
        }
        bot.close().await;
        ingress.close().await;
        relay.close().await;
        listener.close().await;
    }).await;
    assert!(result.is_ok(), "encrypted Opus forwarding timed out");
}

#[test]
fn rtp_sequence_and_timestamp_remain_monotonic_across_playback_replacements() {
    let mut clock = peer::RtpClock::new();
    let first = clock.packet(bytes::Bytes::from_static(b"opus"), true);
    let second = clock.packet(bytes::Bytes::from_static(b"opus"), true);
    assert_eq!(
        second.header.sequence_number,
        first.header.sequence_number.wrapping_add(1)
    );
    assert_eq!(
        second.header.timestamp,
        first.header.timestamp.wrapping_add(960)
    );
    assert!(second.header.marker);
    assert_eq!(second.header.ssrc, 900);
}

/// A short generated WAV goes through the actual ffmpeg -> Opus player. The
/// longer input holds the bounded pipe full while stop kills/reaps ffmpeg.
#[tokio::test]
#[ignore = "requires ffmpeg on PATH"]
async fn ffmpeg_file_streams_and_stops_with_a_full_queue() {
    let path = std::env::temp_dir().join(format!(
        "thiscord-bot-voice-test-{}.wav",
        rand::random::<u64>()
    ));
    struct TempFile(std::path::PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = TempFile(path.clone());
    let samples = 48_000_u32 * 10 * 2;
    let size = samples * 2;
    let mut wav = Vec::with_capacity(size as usize + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(size + 36).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&48_000_u32.to_le_bytes());
    wav.extend_from_slice(&192_000_u32.to_le_bytes());
    wav.extend_from_slice(&4_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    for i in 0..samples {
        wav.extend_from_slice(&((((i / 2) as f32 * 0.04).sin() * 6000.0) as i16).to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    let mut player = media::Player::start(media::Source::File(path));
    let frame = timeout(Duration::from_secs(5), player.frames.recv())
        .await
        .unwrap()
        .expect("ffmpeg audio");
    let mut pcm = [0_i16; 1920];
    assert_eq!(
        opus::Decoder::new(48_000, opus::Channels::Stereo)
            .unwrap()
            .decode(&frame, &mut pcm, false)
            .unwrap(),
        960
    );
    assert!(pcm.iter().any(|sample| sample.abs() > 100));
    timeout(Duration::from_secs(4), player.stop())
        .await
        .unwrap();
    assert!(player.frames.is_closed());
}

#[tokio::test]
async fn signaling_joins_restricted_and_refresh_closes_legacy_session() {
    timeout(Duration::from_secs(20), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (refresh, mut refresh_rx) = mpsc::channel(1);
        let (left, left_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let peer = ReceiverPeer::new().await;
            peer.pc.add_track(peer::track() as Arc<dyn TrackLocal>).await.unwrap();
            let mut member: Participant = serde_json::from_value(serde_json::json!({
                "account_id":"00000000-0000-0000-0000-000000000001", "username":"bot",
                "slot":0,"muted":false,"deafened":false,"can_speak":true
            })).unwrap();
            let mut joined = false;
            let mut admitted = false;
            let mut restricted_seen = false;
            loop {
                let outbound = tokio::select! {
                    frame = socket.next() => {
                        let Some(Ok(Message::Text(text))) = frame else { break; };
                        let frame: ClientFrame = serde_json::from_str(&text).unwrap();
                        assert_eq!(frame.version, VOICE_VERSION);
                        match frame.event {
                            ClientEvent::Join { .. } => {
                                assert!(!joined); joined = true;
                                Some(ServerEvent::Offer {
                                    sdp: serde_json::to_string(&peer.offer().await).unwrap(), slot: 0,
                                    can_speak: true, ice_servers: vec![], packet_redundancy: true,
                                    remote_audio: false, screen_video: false, camera_video: false,
                                    screen_feedback: false, screen_subscriptions: false, screen_layers: false,
                                    resume_token: None,
                                })
                            },
                            ClientEvent::Answer { sdp, packet_redundancy, remote_audio, .. } => {
                                assert!(joined && !admitted);
                                assert!(!packet_redundancy && !remote_audio);
                                peer.pc.set_remote_description(serde_json::from_str(&sdp).unwrap()).await.unwrap();
                                admitted = true;
                                Some(ServerEvent::Participants { members: vec![member.clone()] })
                            },
                            ClientEvent::State { muted, deafened } => {
                                assert!(admitted && muted && deafened);
                                member.muted = muted; member.deafened = deafened;
                                restricted_seen = true;
                                Some(ServerEvent::Participants { members: vec![member.clone()] })
                            },
                            ClientEvent::Ping {} => Some(ServerEvent::Pong {}),
                            ClientEvent::Leave {} => { assert!(restricted_seen); break; },
                            _ => panic!("unexpected client signal"),
                        }
                    },
                    Some(()) = refresh_rx.recv() => Some(ServerEvent::MicrophoneRefreshRequired { challenge: 1 }),
                };
                if let Some(event) = outbound {
                    let text = serde_json::to_string(&ServerFrame { version: VOICE_VERSION, event }).unwrap();
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
            }
            peer.close().await;
            let _ = left.send(());
        });
        let voice = Voice::join(
            &endpoint, "http://localhost", "fixture-token",
            "00000000-0000-0000-0000-000000000002".parse().unwrap(),
            "00000000-0000-0000-0000-000000000003".parse().unwrap(),
        ).await.unwrap();
        assert_eq!(*voice.status().borrow(), PlaybackEvent::Idle);
        let error = voice.play_youtube_video("https://youtu.be/Iq0R9Lmc26c").await.unwrap_err();
        assert!(error.to_string().contains("does not support bot screen sharing"));
        assert_eq!(*voice.status().borrow(), PlaybackEvent::Idle);
        refresh.send(()).await.unwrap();
        let mut status = voice.status();
        status.wait_for(|status| matches!(status, PlaybackEvent::Error(_))).await.unwrap();
        assert!(voice.play_file("anything").await.is_err());
        voice.leave().await.unwrap();
        left_rx.await.unwrap();
        server.await.unwrap();
    }).await.expect("voice signaling fixture timed out");
}
