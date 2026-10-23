//! Production SFU, encrypted two-hop transport. Tags verify exact retained bytes;
//! codec/acoustic recovery is exercised separately by native audio tests.
use super::*;
use thiscord_shared::rtp_red as wire;

async fn collect(peer: &mut Peer, count: usize) -> Vec<rtp::Packet> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut packets = Vec::new();
        while packets.len() < count {
            packets.push(peer.packets.recv().await.unwrap());
        }
        packets
    })
    .await
    .expect("encrypted SFU delivery")
}
async fn no_packet(peer: &mut Peer) {
    assert!(
        tokio::time::timeout(Duration::from_millis(120), peer.packets.recv())
            .await
            .is_err()
    );
}
async fn ack(
    peer: &mut Peer,
    generation: u64,
    challenge: Option<u32>,
    expected: bool,
) -> (u32, u32) {
    ack_ready(peer, generation, challenge, expected, true).await
}
async fn ack_ready(
    peer: &mut Peer,
    generation: u64,
    challenge: Option<u32>,
    expected: bool,
    ready: bool,
) -> (u32, u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match event(&mut peer.socket).await {
                ServerEvent::MicrophoneBoundaryAccepted {
                    generation: actual,
                    challenge: actual_challenge,
                    accepted,
                    source_epoch,
                    microphone_epoch,
                } if actual == generation && actual_challenge == challenge => {
                    assert_eq!(accepted, expected);
                    if accepted {
                        peer.microphone_generation = Some(generation);
                        if let Some(challenge) = challenge {
                            peer.microphone_satisfied = challenge;
                        }
                        if ready && peer.microphone_satisfied != 0 {
                            send(
                                &mut peer.socket,
                                ClientEvent::MicrophoneRefreshReady {
                                    generation,
                                    challenge: peer.microphone_satisfied,
                                },
                            )
                            .await;
                        }
                    }
                    return (source_epoch, microphone_epoch);
                }
                ServerEvent::MicrophoneRefreshRequired { challenge } => {
                    peer.microphone_challenge =
                        Some(peer.microphone_challenge.unwrap_or(0).max(challenge));
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}
async fn full(peer: &mut Peer) -> (u32, u32) {
    let generation = peer.microphone_generation.unwrap() + 1;
    send(
        &mut peer.socket,
        ClientEvent::MicrophoneBoundary { generation },
    )
    .await;
    ack(peer, generation, None, true).await
}
async fn refresh(peer: &mut Peer, minimum: u32) -> (u32, u32, u32) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let challenge = loop {
                if let Some(challenge) = peer
                    .microphone_challenge
                    .take()
                    .filter(|c| *c > peer.microphone_satisfied)
                {
                    break challenge;
                }
                if let ServerEvent::MicrophoneRefreshRequired { challenge } =
                    event(&mut peer.socket).await
                {
                    peer.microphone_challenge = Some(challenge);
                }
            };
            let generation = peer.microphone_generation.unwrap() + 1;
            send(
                &mut peer.socket,
                ClientEvent::MicrophoneRefresh {
                    generation,
                    challenge,
                },
            )
            .await;
            let (source, receiver) = ack(peer, generation, Some(challenge), true).await;
            peer.microphone_satisfied = challenge;
            // A frozen issued challenge must be answered even if a later
            // receiver floor is already known. Its queued maximum follows it.
            if challenge >= minimum {
                return (challenge, source, receiver);
            }
        }
    })
    .await
    .unwrap()
}
async fn raw(peer: &mut Peer, sequence: u16, generation: u64, tag: u8) {
    peer.track
        .write_rtp(rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                ssrc: 900,
                payload_type: 111,
                csrc: vec![(generation >> 32) as u32, generation as u32],
                sequence_number: sequence,
                timestamp: u32::from(sequence) * 960,
                ..Default::default()
            },
            payload: vec![0x78, tag, 0x55].into(),
        })
        .await
        .unwrap();
}

struct RecoveryRoom {
    app: Router,
    state: Value,
    listener_id: AccountId,
    _db: Database,
    owner: String,
    listener: String,
    guild: Value,
    channel: Value,
    addr: std::net::SocketAddr,
    serving: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl RecoveryRoom {
    async fn new() -> Self {
        let db = database();
        let (_, owner) = user(&db, "recovery_owner");
        let (listener_id, listener) = user(&db, "recovery_listener");
        permissions::bootstrap_owner(&db.pool, "recovery_owner").unwrap();
        let app = api::router(
            Some(db.pool.clone()),
            vec!["http://localhost:1420".parse().unwrap()],
        );
        let mut state = command(
            &app,
            &owner,
            json!({"action":"create_guild","name":"Recovery"}),
            StatusCode::OK,
        )
        .await["state"]
            .clone();
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member","username":"recovery_listener"}),
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
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let serving = tokio::spawn(axum::serve(tcp, app.clone()).into_future());
        Self {
            app,
            state,
            listener_id,
            _db: db,
            owner,
            listener,
            guild,
            channel,
            addr,
            serving,
        }
    }
    async fn join(&self, token: &str) -> Socket {
        join(self.addr, token, self.guild.clone(), self.channel.clone()).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn pending_and_invalid_sdp_never_publish_legacy_replacement_rosters() {
    let room = RecoveryRoom::new().await;
    let source = Peer::with_red(room.join(&room.owner).await, 64, true).await;
    let slot = source.slot;
    let mut witness = Peer::new(room.join(&room.listener).await).await;
    // Observe the protected slot before retiring it.
    loop {
        if let ServerEvent::Participants { members } = event(&mut witness.socket).await
            && members
                .iter()
                .any(|m| m.slot == slot && m.microphone_source_epoch != 0)
        {
            break;
        }
    }
    source.close().await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut witness.socket).await
            && members.iter().all(|m| m.slot != slot)
        {
            break;
        }
    }

    let mut pending = room.join(&room.owner).await;
    let ServerEvent::Offer { slot: reused, .. } = event(&mut pending).await else {
        panic!("offer")
    };
    assert_eq!(reused, slot);
    // The normal periodic roster must not classify a pending Answer as legacy.
    loop {
        if let ServerEvent::Participants { members } = event(&mut witness.socket).await {
            assert!(members.iter().all(|m| m.slot != slot));
            break;
        }
    }
    send(
        &mut pending,
        ClientEvent::Answer {
            sdp: json!({"type":"answer","sdp":"v=0\r\n"}).to_string(),
            packet_redundancy: false,
            remote_audio: false,
            screen_feedback: false,
            screen_subscriptions: false,
            screen_layers: false,
        },
    )
    .await;
    // Valid Answer JSON with unusable SDP must never publish negotiated state.
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(frame)) = pending.next().await {
            if matches!(frame, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let replacement = Peer::with_red(room.join(&room.owner).await, 64, true).await;
    assert_eq!(replacement.slot, slot);
    loop {
        if let ServerEvent::Participants { members } = event(&mut witness.socket).await
            && let Some(member) = members.iter().find(|m| m.slot == slot)
        {
            assert_ne!(
                member.microphone_source_epoch, 0,
                "provisional legacy roster escaped"
            );
            assert_ne!(member.microphone_epoch, 0);
            break;
        }
    }
    still_connected(&mut witness.socket).await;
    replacement.close().await;
    witness.close().await;
    room.serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn detached_signaling_recovers_pending_challenge_and_lost_ready() {
    let room = RecoveryRoom::new().await;
    let mut source = Peer::with_red(room.join(&room.owner).await, 64, true).await;
    let mut listener = Peer::with_red(room.join(&room.listener).await, 64, true).await;
    refresh(&mut source, 1).await;
    refresh(&mut listener, 1).await;
    forwarded(&mut source, &mut listener).await;
    let (_, floor) = full(&mut listener).await;
    let challenge = loop {
        if let ServerEvent::MicrophoneRefreshRequired { challenge } =
            event(&mut source.socket).await
            && challenge >= floor
        {
            break challenge;
        }
    };
    // Cut only TCP. Detached ticks must neither use the three retry attempts
    // nor expire this publisher while its 30-second signaling grace is live.
    drop(std::mem::replace(
        &mut source.socket,
        open_voice(room.addr).await,
    ));
    tokio::time::sleep(Duration::from_secs(6)).await;
    let generation = source.microphone_generation.unwrap();
    while listener.packets.try_recv().is_ok() {}
    raw(&mut source, 100, generation, 90).await;
    no_packet(&mut listener).await;
    // Open the actual resume socket now; upgrades have their own five-second
    // authentication deadline, independent of the media owner's recovery grace.
    source.socket = open_voice(room.addr).await;
    resume(
        &mut source.socket,
        &room.owner,
        &room.guild,
        &room.channel,
        &source.resume_token,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut source.socket).await,
        ServerEvent::Resumed {}
    ));
    assert!(
        matches!(event(&mut source.socket).await, ServerEvent::MicrophoneRefreshRequired { challenge: c } if c == challenge)
    );
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefresh {
            generation: generation + 1,
            challenge,
        },
    )
    .await;
    ack_ready(&mut source, generation + 1, Some(challenge), true, false).await;

    // ACK was processed, but Ready never reached the server. Recovery must
    // reissue that same satisfied challenge, so the client can finish the proof.
    drop(std::mem::replace(
        &mut source.socket,
        open_voice(room.addr).await,
    ));
    tokio::time::sleep(Duration::from_secs(6)).await;
    source.socket = open_voice(room.addr).await;
    resume(
        &mut source.socket,
        &room.owner,
        &room.guild,
        &room.channel,
        &source.resume_token,
        2,
    )
    .await;
    assert!(matches!(
        event(&mut source.socket).await,
        ServerEvent::Resumed {}
    ));
    assert!(
        matches!(event(&mut source.socket).await, ServerEvent::MicrophoneRefreshRequired { challenge: c } if c == challenge)
    );
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefresh {
            generation: generation + 1,
            challenge,
        },
    )
    .await;
    ack(&mut source, generation + 1, Some(challenge), true).await;
    // Allow more than the ordinary Ready deadline, then require real RTP.
    tokio::time::sleep(Duration::from_secs(5)).await;
    source.sequence = 100;
    forwarded(&mut source, &mut listener).await;
    forwarded(&mut listener, &mut source).await;
    source.close().await;
    listener.close().await;
    room.serving.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn negotiated_red_crosses_encrypted_sfu_with_loss_reorder_and_legacy_fallback() {
    let db = database();
    let (_, owner) = user(&db, "red_owner");
    let (_, modern_token) = user(&db, "red_modern");
    let (_, legacy_token) = user(&db, "red_legacy");
    let (_, churn_token) = user(&db, "red_churn");
    permissions::bootstrap_owner(&db.pool, "red_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"RED"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for username in ["red_modern", "red_legacy", "red_churn"] {
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
    let mut source = Peer::with_red(
        join(addr, &owner, guild.clone(), channel.clone()).await,
        64,
        true,
    )
    .await;
    let mut modern = Peer::with_red(
        join(addr, &modern_token, guild.clone(), channel.clone()).await,
        64,
        true,
    )
    .await;
    let mut legacy =
        Peer::new(join(addr, &legacy_token, guild.clone(), channel.clone()).await).await;
    refresh(&mut source, 1).await;
    refresh(&mut modern, 1).await;
    let legacy_packet = forwarded(&mut legacy, &mut modern).await;
    assert!(
        legacy_packet.header.csrc.is_empty(),
        "legacy source must not advertise strict privacy/DRED"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerEvent::Participants { members } = event(&mut modern.socket).await
                && let Some(info) = members.iter().find(|m| m.slot == legacy.slot)
            {
                assert_eq!(info.microphone_source_epoch, 0);
                assert_eq!(info.microphone_epoch, 0);
                break;
            }
        }
    })
    .await
    .unwrap();
    forwarded(&mut source, &mut modern).await;
    forwarded(&mut source, &mut legacy).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    while modern.packets.try_recv().is_ok() {}
    while legacy.packets.try_recv().is_ok() {}
    let base = source.sequence.wrapping_add(1);
    let generation = source.microphone_generation.unwrap();
    let frames: Vec<_> = (0..12u8).map(|n| vec![0x78, n, 0x55]).collect();
    let order = [0, 1, 2, 5, 4, 6, 9, 10, 11]; // isolated3, burst7/8, reordered5/4
    for n in order {
        let history: Vec<_> = (1..=2)
            .rev()
            .filter(|age| n >= *age)
            .map(|age| wire::Block {
                offset: age as u16 * 960,
                payload: frames[n - age].as_slice(),
            })
            .collect();
        source
            .track
            .write_rtp(rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    ssrc: 900,
                    payload_type: wire::PAYLOAD_TYPE,
                    csrc: vec![(generation >> 32) as u32, generation as u32],
                    sequence_number: base.wrapping_add(n as u16),
                    timestamp: (u32::from(base) + n as u32) * 960,
                    ..Default::default()
                },
                payload: wire::encode(&frames[n], &history).unwrap().into(),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let protected = collect(&mut modern, order.len()).await;
    let fallback = collect(&mut legacy, order.len()).await;
    let mut recovered = [false; 12];
    let mut primary = [false; 12];
    for packet in &protected {
        assert_eq!(packet.header.payload_type, wire::PAYLOAD_TYPE);
        assert_eq!(packet.header.csrc.len(), 2);
        let red = wire::parse(&packet.payload).unwrap();
        let index = usize::from(red.primary[1]);
        recovered[index] = true;
        for block in red.history {
            let previous = usize::from(block.payload[1]);
            assert_eq!((index - previous) * 960, usize::from(block.offset));
            assert_eq!(block.payload, frames[previous]);
            recovered[previous] = true;
        }
    }
    for packet in fallback {
        assert_eq!(packet.header.payload_type, wire::OPUS_PAYLOAD_TYPE);
        let index = usize::from(packet.payload[1]);
        assert_eq!(packet.payload.as_ref(), frames[index]);
        primary[index] = true;
    }
    assert_eq!(recovered.iter().filter(|v| **v).count(), 12);
    assert_eq!(primary.iter().filter(|v| **v).count(), 9);
    source
        .track
        .write_rtp(rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                ssrc: 900,
                payload_type: wire::PAYLOAD_TYPE,
                csrc: vec![0, generation as u32],
                sequence_number: base.wrapping_add(12),
                timestamp: (u32::from(base) + 12) * 960,
                ..Default::default()
            },
            payload: vec![0xff, 0xff].into(),
        })
        .await
        .unwrap();
    no_packet(&mut modern).await;
    assert!(legacy.packets.try_recv().is_err());

    // Hold old publisher->SFU UDP until after the listener has ACKed its new
    // barrier. It still has the accepted sender generation, but must be gated.
    let (_, floor) = full(&mut modern).await;
    raw(&mut source, base.wrapping_add(13), generation, 90).await;
    no_packet(&mut modern).await;
    assert_eq!(
        collect(&mut legacy, 1).await[0].payload[1],
        90,
        "only affected listener should be blocked"
    );
    // A generic fresh generation could have been preissued before that barrier.
    let (_, source_receive_floor) = full(&mut source).await;
    // A silent/muted publisher responds to control challenges without any RTP.
    refresh(&mut modern, source_receive_floor).await;
    let preissued = source.microphone_generation.unwrap();
    raw(&mut source, base.wrapping_add(14), preissued, 91).await;
    no_packet(&mut modern).await;
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 91);
    let (challenge, source_epoch, receive_floor) = refresh(&mut source, floor).await;
    assert_eq!(
        receive_floor, source_receive_floor,
        "source refresh caused recursive listener barrier"
    );
    assert!(source_epoch > floor);
    let refreshed = source.microphone_generation.unwrap();
    raw(&mut source, base.wrapping_add(15), generation, 92).await;
    raw(&mut source, base.wrapping_add(16), preissued, 93).await;
    raw(&mut source, base.wrapping_add(17), refreshed, 94).await;
    let resumed = collect(&mut modern, 1).await;
    assert_eq!(resumed[0].payload[1], 94);
    assert_eq!(resumed[0].header.csrc[0], protected[0].header.csrc[0]);
    assert_eq!(resumed[0].header.csrc[1], source_epoch);
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 94);
    no_packet(&mut modern).await;
    // A duplicate proof is idempotent; a superseded challenge cannot satisfy
    // the next listener barrier even when paired with a fresh numeric tag.
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefresh {
            generation: refreshed,
            challenge,
        },
    )
    .await;
    assert_eq!(
        ack(&mut source, refreshed, Some(challenge), true).await,
        (source_epoch, receive_floor)
    );
    let (_, latest_floor) = full(&mut modern).await;
    // Only an actually superseded issued challenge is stale. A fresh reproof
    // of the current satisfied challenge is allowed for lost-ACK recovery.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerEvent::MicrophoneRefreshRequired { challenge: next } =
                event(&mut source.socket).await
                && next > challenge
            {
                source.microphone_challenge = Some(next);
                break;
            }
        }
    })
    .await
    .unwrap();
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefresh {
            generation: refreshed + 1,
            challenge,
        },
    )
    .await;
    ack(&mut source, refreshed + 1, Some(challenge), false).await;
    raw(&mut source, base.wrapping_add(18), refreshed, 95).await;
    no_packet(&mut modern).await;
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 95);
    refresh(&mut source, latest_floor).await;
    let final_generation = source.microphone_generation.unwrap();
    raw(&mut source, base.wrapping_add(19), final_generation, 96).await;
    assert_eq!(collect(&mut modern, 1).await[0].payload[1], 96);
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 96);
    let interrupted = std::time::Instant::now();
    let (_, measured_floor) = full(&mut modern).await;
    refresh(&mut source, measured_floor).await;
    let measured_generation = source.microphone_generation.unwrap();
    raw(&mut source, base.wrapping_add(20), measured_generation, 97).await;
    assert_eq!(collect(&mut modern, 1).await[0].payload[1], 97);
    println!(
        "SFU privacy barrier + challenge + first fresh RTP: {:.3} ms (loopback; includes challenge cadence, excludes devices/DSP)",
        interrupted.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 97);
    // A slow/batched signaling consumer must get actual RTP service after it
    // processes the ACK, even if another listener requests a newer barrier.
    let mut churn = Peer::with_red(join(addr, &churn_token, guild, channel).await, 64, true).await;
    refresh(&mut modern, 1).await;
    refresh(&mut churn, 1).await;
    let issued = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerEvent::MicrophoneRefreshRequired { challenge } =
                event(&mut source.socket).await
                && challenge > source.microphone_satisfied
            {
                break challenge;
            }
        }
    })
    .await
    .unwrap();
    let delayed_generation = source.microphone_generation.unwrap() + 1;
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefresh {
            generation: delayed_generation,
            challenge: issued,
        },
    )
    .await;
    let (_, churn_floor) = full(&mut churn).await;
    assert!(churn_floor > issued);
    tokio::time::sleep(Duration::from_millis(400)).await;
    ack_ready(&mut source, delayed_generation, Some(issued), true, false).await;
    // Still no Ready: more than the old acceptance-based cooldown has passed.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(
        &mut source.socket,
        ClientEvent::MicrophoneRefreshReady {
            generation: delayed_generation,
            challenge: issued,
        },
    )
    .await;
    let service_started = std::time::Instant::now();
    raw(&mut source, base.wrapping_add(21), delayed_generation, 98).await;
    assert_eq!(collect(&mut modern, 1).await[0].payload[1], 98);
    assert!(
        service_started.elapsed() < Duration::from_millis(250),
        "fresh encrypted RTP missed the guaranteed service interval"
    );
    assert_eq!(collect(&mut legacy, 1).await[0].payload[1], 98);
    no_packet(&mut churn).await;
    // Read buffered messages too: a wrongly preissued next challenge would
    // already be queued beside the delayed ACK and fail this assertion.
    while service_started.elapsed() < Duration::from_millis(200) {
        let remaining = Duration::from_millis(200).saturating_sub(service_started.elapsed());
        match tokio::time::timeout(remaining, event(&mut source.socket)).await {
            Ok(ServerEvent::MicrophoneRefreshRequired { challenge }) => {
                assert!(
                    challenge <= issued,
                    "next challenge preceded client-ready service interval"
                );
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    refresh(&mut source, churn_floor).await;
    churn.close().await;
    source.close().await;
    modern.close().await;
    legacy.close().await;
    serving.abort();
}

// Real Opus, including embedded DRED history, over the production encrypted SFU.
fn tone(encoder: &mut opus::Encoder, index: usize) -> Vec<u8> {
    let pcm: [f32; 960] = std::array::from_fn(|i| {
        ((index * 960 + i) as f32 * std::f32::consts::TAU * 440.0 / 48000.0).sin() * 0.2
    });
    let mut bytes = [0; 1100];
    let len = encoder.encode_float(&pcm, &mut bytes).unwrap();
    bytes[..len].to_vec()
}
async fn audio(peer: &mut Peer, generation: u64, primary: &[u8], history: &[u8], red: bool) {
    peer.sequence = peer.sequence.wrapping_add(1);
    let history = if history.is_empty() {
        vec![]
    } else {
        vec![wire::Block {
            offset: 960,
            payload: history,
        }]
    };
    peer.track
        .write_rtp(rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                ssrc: 900,
                payload_type: if red { wire::PAYLOAD_TYPE } else { 111 },
                csrc: vec![(generation >> 32) as u32, generation as u32],
                sequence_number: peer.sequence,
                timestamp: u32::from(peer.sequence) * 960,
                ..Default::default()
            },
            payload: if red {
                wire::encode(primary, &history).unwrap()
            } else {
                primary.to_vec()
            }
            .into(),
        })
        .await
        .unwrap();
}
async fn moderation_roster(peer: &mut Peer, revision: u64) -> voice::Participant {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match event(&mut peer.socket).await {
                ServerEvent::Participants { members } => {
                    if let Some(own) = members
                        .into_iter()
                        .find(|m| m.slot == peer.slot && m.publication_revision >= revision)
                    {
                        return own;
                    }
                }
                ServerEvent::MicrophoneRefreshRequired { challenge } => {
                    peer.microphone_challenge =
                        Some(peer.microphone_challenge.unwrap_or(0).max(challenge))
                }
                ServerEvent::Revoked {} | ServerEvent::Error { .. } => {
                    panic!("moderation must retain the connection")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn moderator_boundaries_reject_encrypted_restricted_red_and_dred_history() {
    for (red_enabled, dred) in [(true, false), (false, true), (true, true)] {
        let mut room = RecoveryRoom::new().await;
        let mut source = Peer::with_red(room.join(&room.owner).await, 64, true).await;
        let mut target = Peer::with_red(room.join(&room.listener).await, 64, true).await;
        refresh(&mut source, 1).await;
        refresh(&mut target, 1).await;
        forwarded(&mut source, &mut target).await;
        forwarded(&mut target, &mut source).await;
        let mut encoder =
            opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).unwrap();
        encoder
            .set_bitrate(opus::Bitrate::Bits(if dred { 40_000 } else { 32_000 }))
            .unwrap();
        encoder.set_vbr(dred).unwrap();
        encoder.set_inband_fec(true).unwrap();
        encoder.set_signal(opus::Signal::Voice).unwrap();
        encoder
            .set_packet_loss_perc(if dred { 20 } else { 5 })
            .unwrap();
        encoder.set_dred_duration(if dred { 6 } else { 0 }).unwrap();
        for action in ["set_server_mute", "set_server_deafen"] {
            let key = if action == "set_server_mute" {
                "muted"
            } else {
                "deafened"
            };
            change(
                &room.app,
                &room.owner,
                &mut room.state,
                json!({"action":action,"account_id":room.listener_id,key:true}),
                StatusCode::OK,
            )
            .await;
            let revision = if action == "set_server_mute" { 1 } else { 3 };
            let own = moderation_roster(&mut target, revision).await;
            refresh(&mut source, own.microphone_epoch).await;
            refresh(&mut target, own.microphone_epoch).await;
            let mut restricted = None;
            for n in 0..80 {
                // Match the native encoder's per-frame DRED policy after resets.
                encoder.set_dred_duration(if dred { 6 } else { 0 }).unwrap();
                let packet = tone(&mut encoder, n);
                // Opus may omit DRED on individual packets despite the CTL.
                // Require actual embedded history, not merely an enabled setting.
                if !dred || opus::Dred::parse(&packet, 2880, 48000).unwrap().is_some() {
                    restricted = Some(packet);
                }
            }
            let restricted = restricted.expect("restricted audio must contain real redundancy");
            let history = if dred { &[][..] } else { restricted.as_slice() };
            tokio::time::sleep(Duration::from_millis(100)).await;
            while target.packets.try_recv().is_ok() {}
            while source.packets.try_recv().is_ok() {}
            let old_source = source.microphone_generation.unwrap();
            let old_target = target.microphone_generation.unwrap();
            audio(&mut target, old_target, &restricted, history, red_enabled).await;
            no_packet(&mut source).await;
            if action == "set_server_deafen" {
                audio(&mut source, old_source, &restricted, history, red_enabled).await;
                no_packet(&mut target).await;
            }
            change(
                &room.app,
                &room.owner,
                &mut room.state,
                json!({"action":action,"account_id":room.listener_id,key:false}),
                StatusCode::OK,
            )
            .await;
            let own = moderation_roster(&mut target, revision + 1).await;
            // Fresh outer RTP timestamps still contain history from restriction.
            // Neither direction may open before the matching refresh proof.
            audio(&mut source, old_source, &restricted, history, red_enabled).await;
            audio(&mut target, old_target, &restricted, history, red_enabled).await;
            no_packet(&mut target).await;
            no_packet(&mut source).await;
            refresh(&mut source, own.microphone_epoch).await;
            refresh(&mut target, own.microphone_epoch).await;
            // Delayed old-tag UDP cannot acquire the newly admitted floor.
            audio(&mut source, old_source, &restricted, history, red_enabled).await;
            audio(&mut target, old_target, &restricted, history, red_enabled).await;
            no_packet(&mut target).await;
            no_packet(&mut source).await;
            encoder.reset_state().unwrap();
            let fresh = tone(&mut encoder, 101);
            assert_ne!(fresh, restricted);
            let sg = source.microphone_generation.unwrap();
            let tg = target.microphone_generation.unwrap();
            audio(&mut source, sg, &fresh, history, red_enabled).await;
            audio(&mut target, tg, &fresh, history, red_enabled).await;
            for packet in [
                collect(&mut source, 1).await.remove(0),
                collect(&mut target, 1).await.remove(0),
            ] {
                let primary = if red_enabled {
                    let parsed = wire::parse(&packet.payload).unwrap();
                    assert!(
                        parsed.history.is_empty(),
                        "restricted RED history crossed a moderation floor"
                    );
                    parsed.primary
                } else {
                    packet.payload.as_ref()
                };
                assert_eq!(primary, fresh);
                assert_ne!(primary, restricted);
            }
        }
        source.close().await;
        target.close().await;
        room.serving.abort();
    }
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn coalesced_committed_moderation_retires_screen_and_camera_until_explicit_restart() {
    let mut room = RecoveryRoom::new().await;
    let mut target = Peer::new(room.join(&room.listener).await).await;
    send(
        &mut target.socket,
        ClientEvent::Screen {
            active: true,
            audio: true,
            quality: None,
        },
    )
    .await;
    send(&mut target.socket, ClientEvent::Camera { active: true }).await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut target.socket).await
            && members
                .iter()
                .any(|m| m.slot == target.slot && m.sharing_screen && m.camera_active)
        {
            break;
        }
    }
    // No await on this current-thread runtime: two separate committed changes
    // precede any socket revalidation. The final HTTP no-op wakes access checks.
    let mut c = room._db.pool.get().unwrap();
    for restricted in [true, false] {
        c.transaction::<_,diesel::result::Error,_>(|c| {
            diesel::sql_query("SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE").bind::<Text,_>(room.guild.as_str().unwrap()).execute(c)?;
            diesel::sql_query("INSERT INTO guild_moderation(guild_id,account_id,server_deafened) VALUES($1::uuid,$2::uuid,$3::boolean) ON CONFLICT(guild_id,account_id) DO UPDATE SET server_deafened=EXCLUDED.server_deafened")
                .bind::<Text,_>(room.guild.as_str().unwrap()).bind::<Text,_>(room.listener_id.to_string()).bind::<Text,_>(restricted.to_string()).execute(c)?;
            Ok(())
        }).unwrap();
    }
    drop(c);
    change(
        &room.app,
        &room.owner,
        &mut room.state,
        json!({"action":"set_server_deafen","account_id":room.listener_id,"deafened":false}),
        StatusCode::OK,
    )
    .await;
    let own = moderation_roster(&mut target, 2).await;
    assert_eq!(
        own.publication_revision, 2,
        "no-op must not create another edge"
    );
    assert!(!own.server_deafened && !own.server_muted);
    assert!(!own.sharing_screen && !own.sharing_audio && !own.camera_active);
    send(
        &mut target.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    still_connected(&mut target.socket).await;
    let still = moderation_roster(&mut target, 2).await;
    assert!(!still.sharing_screen && !still.camera_active);
    send(
        &mut target.socket,
        ClientEvent::Screen {
            active: true,
            audio: true,
            quality: None,
        },
    )
    .await;
    send(&mut target.socket, ClientEvent::Camera { active: true }).await;
    loop {
        let next = moderation_roster(&mut target, 2).await;
        if next.sharing_screen && next.camera_active {
            assert!(next.screen_epoch > own.screen_epoch && next.camera_epoch > own.camera_epoch);
            break;
        }
    }
    target.close().await;
    room.serving.abort();
}
