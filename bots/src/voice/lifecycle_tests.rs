//! Linux process observations supplement the platform-independent protocol tests.
//! These inspect only this test process's own child PIDs, never unrelated jobs.
use super::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

struct ServerTask(tokio::task::JoinHandle<()>);
impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Fixture {
    voice: Option<Voice>,
    peer: ReceiverPeer,
    signals: mpsc::Sender<ServerEvent>,
    hold_unmute: Arc<AtomicBool>,
    queue_stale_rosters: Arc<AtomicBool>,
    stale_rosters_sent: watch::Receiver<bool>,
    release_restriction: mpsc::Sender<()>,
    _server: ServerTask,
}

fn member() -> Participant {
    serde_json::from_value(serde_json::json!({
        "account_id":"00000000-0000-0000-0000-000000000001", "username":"bot",
        "slot":0,"muted":false,"deafened":false,"can_speak":true
    }))
    .unwrap()
}

impl Fixture {
    async fn join() -> Self {
        Self::join_with_member(member()).await
    }

    async fn join_with_member(mut member: Participant) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let peer = ReceiverPeer::new().await;
        peer.pc
            .add_track(peer::track() as Arc<dyn TrackLocal>)
            .await
            .unwrap();
        let offer = serde_json::to_string(&peer.offer().await).unwrap();
        let pc = peer.pc.clone();
        let (signals, mut incoming) = mpsc::channel(8);
        let hold_unmute = Arc::new(AtomicBool::new(false));
        let hold = hold_unmute.clone();
        let queue_stale_rosters = Arc::new(AtomicBool::new(false));
        let queue_stale = queue_stale_rosters.clone();
        let (stale_sent, stale_rosters_sent) = watch::channel(false);
        let (release_restriction, mut release) = mpsc::channel(1);
        let server = ServerTask(tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let mut offer = Some(offer);
            let mut probing = false;
            let mut held_pongs = 0;
            loop {
                let event = tokio::select! {
                    frame = socket.next() => {
                        let Some(Ok(Message::Text(text))) = frame else { break; };
                        let frame: ClientFrame = serde_json::from_str(&text).unwrap();
                        assert_eq!(frame.version, VOICE_VERSION);
                        match frame.event {
                            ClientEvent::Join { .. } => Some(ServerEvent::Offer {
                                sdp: offer.take().expect("only one join"), slot: 0,
                                can_speak: true, ice_servers: vec![], packet_redundancy: true,
                                remote_audio: false, screen_video: false, camera_video: false,
                                screen_feedback: false, screen_subscriptions: false, screen_layers: false,
                                resume_token: None,
                            }),
                            ClientEvent::Answer { sdp, packet_redundancy, remote_audio, .. } => {
                                assert!(!packet_redundancy && !remote_audio);
                                let answer: RTCSessionDescription = serde_json::from_str(&sdp).unwrap();
                                assert!(answer.sdp.contains("a=sendonly"));
                                assert!(!answer.sdp.contains("a=recvonly") && !answer.sdp.contains("a=sendrecv"));
                                pc.set_remote_description(answer).await.unwrap();
                                Some(ServerEvent::Participants { members: vec![member.clone()] })
                            },
                            ClientEvent::State { muted, deafened } => {
                                assert_eq!(muted, deafened);
                                assert!(muted || !probing, "unmute requested before the restriction fence");
                                let changed = member.muted != muted || member.deafened != deafened;
                                member.muted = muted; member.deafened = deafened;
                                if muted && queue_stale.swap(false, Ordering::AcqRel) {
                                    probing = true;
                                    // A matching old restricted snapshot followed
                                    // by the previous playback's unmuted roster.
                                    // Neither acknowledges this mute request.
                                    for old_restriction in [true, false] {
                                        let mut stale = member.clone();
                                        stale.muted = old_restriction; stale.deafened = old_restriction;
                                        server_send(&mut socket, ServerEvent::Participants { members: vec![stale] }).await;
                                    }
                                    stale_sent.send_replace(true);
                                    None
                                } else if !changed || !muted && hold.load(Ordering::Acquire) { None }
                                else { Some(ServerEvent::Participants { members: vec![member.clone()] }) }
                            },
                            ClientEvent::Ping {} => {
                                if probing { held_pongs += 1; None } else { Some(ServerEvent::Pong {}) }
                            },
                            ClientEvent::Leave {} => break,
                            _ => panic!("unexpected signal"),
                        }
                    },
                    event = incoming.recv() => {
                        if let Some(ServerEvent::Participants { members }) = &event {
                            member = members[0].clone();
                        }
                        event
                    },
                    Some(()) = release.recv() => {
                        assert!(probing && held_pongs > 0);
                        assert!(member.muted && member.deafened);
                        server_send(&mut socket, ServerEvent::Participants { members: vec![member.clone()] }).await;
                        for _ in 0..held_pongs { server_send(&mut socket, ServerEvent::Pong {}).await; }
                        probing = false;
                        held_pongs = 0;
                        None
                    },
                };
                if let Some(event) = event {
                    let text = serde_json::to_string(&ServerFrame {
                        version: VOICE_VERSION,
                        event,
                    })
                    .unwrap();
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            }
            // Keep the receiving peer alive in Fixture after signaling closes:
            // observing silence must prove the client stopped, not hide RTP by
            // destroying the receiver when Leave or Revoked is handled.
        }));
        let voice = Voice::join(
            &endpoint,
            "http://localhost",
            "fixture-token",
            "00000000-0000-0000-0000-000000000002".parse().unwrap(),
            "00000000-0000-0000-0000-000000000003".parse().unwrap(),
        )
        .await
        .unwrap();
        Self {
            voice: Some(voice),
            peer,
            signals,
            hold_unmute,
            queue_stale_rosters,
            stale_rosters_sent,
            release_restriction,
            _server: server,
        }
    }

    fn voice(&self) -> &Voice {
        self.voice.as_ref().unwrap()
    }

    async fn hold_commands(&self) {
        self.voice().0.hold_commands.store(1, Ordering::Release);
        timeout(Duration::from_secs(3), async {
            while self.voice().0.hold_commands.load(Ordering::Acquire) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session must acknowledge the command dequeue barrier");
    }

    async fn packet(&mut self) -> rtp::Packet {
        timeout(Duration::from_secs(5), self.peer.packets.recv())
            .await
            .expect("active playback must produce RTP")
            .expect("receiver stays open")
    }

    async fn silence(&mut self) {
        // Drain packets already in flight before the stop/revoke acknowledgement,
        // then observe many complete 20 ms send intervals with no new RTP.
        let settle = tokio::time::sleep(Duration::from_millis(150));
        tokio::pin!(settle);
        loop {
            tokio::select! {
                _ = &mut settle => break,
                packet = self.peer.packets.recv() => { assert!(packet.is_some()); },
            }
        }
        assert!(
            timeout(Duration::from_millis(300), self.peer.packets.recv())
                .await
                .is_err(),
            "RTP continued after playback was stopped"
        );
    }

    async fn playing(&mut self, wav: &Wav) -> (u32, rtp::Packet) {
        timeout(Duration::from_secs(3), self.voice().play_file(&wav.0))
            .await
            .unwrap()
            .unwrap();
        let packet = self.packet().await;
        assert_eq!(*self.voice().status().borrow(), PlaybackEvent::Playing);
        (decoder_pid(&wav.0).await, packet)
    }
}

async fn server_send(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    event: ServerEvent,
) {
    let text = serde_json::to_string(&ServerFrame {
        version: VOICE_VERSION,
        event,
    })
    .unwrap();
    socket.send(Message::Text(text.into())).await.unwrap();
}

struct Wav(PathBuf);
impl Wav {
    fn new(silent: bool) -> Self {
        let path = std::env::temp_dir().join(format!(
            "thiscord-voice-lifecycle-{}.wav",
            rand::random::<u64>()
        ));
        // Thirty seconds ensures ffmpeg is still blocked on its bounded output
        // during the assertions, not naturally finished before we check cleanup.
        let samples = 48_000_u32 * 30 * 2;
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
            let sample = if silent {
                0
            } else {
                (((i / 2) as f32 * 0.04).sin() * 6000.0) as i16
            };
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        Self(path)
    }
}
impl Drop for Wav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn find_decoder(path: &Path) -> Option<u32> {
    use std::os::unix::ffi::OsStrExt;
    let mut children = BTreeSet::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        if let Ok(pids) = std::fs::read_to_string(task.path().join("children")) {
            children.extend(
                pids.split_whitespace()
                    .filter_map(|pid| pid.parse::<u32>().ok()),
            );
        }
    }
    children.into_iter().find(|pid| {
        std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|command| {
            command
                .split(|byte| *byte == 0)
                .any(|arg| arg == path.as_os_str().as_bytes())
        })
    })
}

async fn decoder_pid(path: &Path) -> u32 {
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(pid) = find_decoder(path) {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("active ffmpeg child PID was not found")
}

async fn reaped(pid: u32) {
    timeout(Duration::from_secs(3), async {
        while Path::new(&format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ffmpeg child must be killed AND reaped, including zombies");
}

#[derive(Clone, Copy, Debug)]
enum PlaybackPhase {
    Active,
    AwaitingUnmute,
    PendingFence,
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_moderation_invalidates_buffered_and_blocked_play_before_release_dequeue() {
    timeout(Duration::from_secs(35), async {
        let first = Wav::new(false);
        let stale = Wav::new(true);
        for restriction in ["mute", "deafen", "coalesced"] {
            let mut fixture = Fixture::join().await;
            let (old_pid, _) = fixture.playing(&first).await;
            let voice = fixture.voice().clone();
            let mut publication = voice.0.publication.clone();
            let before = *publication.borrow_and_update();
            fixture.hold_commands().await;

            // Poll the actual public API to enqueue, but keep its reply pending.
            // The session still consumes moderation/release and reaps FFmpeg.
            let mut queued = Box::pin(voice.play_file(&stale.0));
            assert!(futures_util::poll!(&mut queued).is_pending());
            let mut stop = Box::pin(voice.stop());
            assert!(futures_util::poll!(&mut stop).is_pending());

            let mut restricted = member();
            match restriction {
                "mute" => restricted.server_muted = true,
                "deafen" => restricted.server_deafened = true,
                "coalesced" => restricted.publication_revision = 2,
                _ => unreachable!(),
            }
            fixture
                .signals
                .send(ServerEvent::Participants {
                    members: vec![restricted.clone()],
                })
                .await
                .unwrap();
            publication
                .wait_for(|state| state.generation > before.generation)
                .await
                .unwrap();
            let during = *publication.borrow();
            let mut status = voice.status();
            status
                .wait_for(|event| *event == PlaybackEvent::Stopped)
                .await
                .unwrap();
            reaped(old_pid).await;

            // Fill the bounded queue with real requests issued while restricted.
            // For the coalesced snapshot, all old stamps predate the revision.
            let mut restricted_plays = Vec::new();
            let mut old_replies = Vec::new();
            for _ in 0..6 {
                if restriction != "coalesced" {
                    let mut play = Box::pin(voice.play_file(&stale.0));
                    assert!(futures_util::poll!(&mut play).is_pending());
                    restricted_plays.push(play);
                } else {
                    let (reply, result) = oneshot::channel();
                    voice
                        .0
                        .commands
                        .try_send(Command::Play(
                            media::Source::File(stale.0.clone()),
                            before,
                            reply,
                        ))
                        .unwrap_or_else(|_| panic!("queue has room"));
                    old_replies.push(result);
                }
            }
            assert_eq!(voice.0.commands.capacity(), 0);
            assert!(
                voice.play_file(&stale.0).await.is_err(),
                "public overload stays bounded"
            );

            // Simulate a sender preempted after issuance, including a blocked
            // channel send. Draining only the present queue cannot revoke it.
            let (reply, delayed_reply) = oneshot::channel();
            let mut delayed_send = Box::pin(voice.0.commands.send(Command::Play(
                media::Source::File(stale.0.clone()),
                before,
                reply,
            )));
            assert!(futures_util::poll!(&mut delayed_send).is_pending());

            if restriction != "coalesced" {
                restricted.server_muted = false;
                restricted.server_deafened = false;
                restricted.muted = true;
                restricted.deafened = true;
                fixture
                    .signals
                    .send(ServerEvent::Participants {
                        members: vec![restricted],
                    })
                    .await
                    .unwrap();
                publication
                    .wait_for(|state| !state.restricted)
                    .await
                    .unwrap();
            }
            // Release (or the coalesced boundary) was definitely consumed first;
            // every stale Play is still buffered or blocked at this point.
            assert_eq!(voice.0.commands.capacity(), 0);
            voice.0.hold_commands.store(0, Ordering::Release);
            assert!(queued.await.is_err());
            stop.await.unwrap();
            for play in restricted_plays {
                assert!(play.await.is_err());
            }
            for reply in old_replies {
                assert!(reply.await.unwrap().is_err());
            }
            delayed_send
                .await
                .unwrap_or_else(|_| panic!("session stays open"));
            assert!(delayed_reply.await.unwrap().is_err());
            fixture.silence().await;
            assert!(find_decoder(&stale.0).is_none());
            assert_eq!(*status.borrow(), PlaybackEvent::Stopped);

            let (fresh_pid, _) = fixture.playing(&first).await;
            // A late restricted request must also leave a newer valid Play alone.
            let (reply, result) = oneshot::channel();
            let old = if restriction == "coalesced" {
                before
            } else {
                during
            };
            voice
                .0
                .commands
                .send(Command::Play(
                    media::Source::File(stale.0.clone()),
                    old,
                    reply,
                ))
                .await
                .unwrap_or_else(|_| panic!("session stays open"));
            assert!(result.await.unwrap().is_err());
            assert_eq!(*status.borrow(), PlaybackEvent::Playing);
            fixture.packet().await;
            assert!(find_decoder(&stale.0).is_none());
            voice.stop().await.unwrap();
            reaped(fresh_pid).await;
            fixture.silence().await;
            voice.leave().await.unwrap();
        }
    })
    .await
    .expect("buffered Play moderation lifecycle timed out");
}

async fn moderation_cancels_playback(phase: PlaybackPhase) {
    timeout(Duration::from_secs(40), async {
        let first = Wav::new(false);
        let replacement = Wav::new(true);
        // Exercise independent flags without a revision change (older servers),
        // and a revision-only snapshot after a coalesced restrict/release.
        for restriction in ["mute", "deafen", "coalesced"] {
            let mut initial = member();
            initial.publication_revision = 9;
            let mut fixture = Fixture::join_with_member(initial.clone()).await;
            let (old_pid, _) = fixture.playing(&first).await;
            let active_pid = match phase {
                PlaybackPhase::Active => Some(old_pid),
                PlaybackPhase::AwaitingUnmute => {
                    fixture.hold_unmute.store(true, Ordering::Release);
                    fixture.voice().play_file(&replacement.0).await.unwrap();
                    reaped(old_pid).await;
                    let pid = decoder_pid(&replacement.0).await;
                    fixture.silence().await;
                    assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Starting);
                    Some(pid)
                }
                PlaybackPhase::PendingFence => {
                    fixture.queue_stale_rosters.store(true, Ordering::Release);
                    fixture.voice().play_file(&replacement.0).await.unwrap();
                    let mut queued = fixture.stale_rosters_sent.clone();
                    timeout(Duration::from_secs(3), queued.wait_for(|value| *value))
                        .await
                        .unwrap()
                        .unwrap();
                    reaped(old_pid).await;
                    fixture.silence().await;
                    assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Starting);
                    assert!(find_decoder(&replacement.0).is_none());
                    None
                }
            };
            let mut restricted_member = initial.clone();
            // The pending command is still self-restricted; an awaiting-unmute
            // player and active player have already cleared their self flags.
            if matches!(phase, PlaybackPhase::PendingFence) {
                restricted_member.muted = true;
                restricted_member.deafened = true;
            }
            match restriction {
                "mute" => restricted_member.server_muted = true,
                "deafen" => restricted_member.server_deafened = true,
                "coalesced" => restricted_member.publication_revision += 2,
                _ => unreachable!(),
            }
            fixture
                .signals
                .send(ServerEvent::Participants {
                    members: vec![restricted_member.clone()],
                })
                .await
                .unwrap();
            let mut status = fixture.voice().status();
            timeout(
                Duration::from_secs(5),
                status.wait_for(|event| *event == PlaybackEvent::Stopped),
            )
            .await
            .expect("moderation must cancel active AND starting playback")
            .unwrap();
            if let Some(pid) = active_pid {
                reaped(pid).await;
            }
            fixture.silence().await;
            assert!(find_decoder(&first.0).is_none());
            assert!(find_decoder(&replacement.0).is_none());
            if restriction != "coalesced" {
                assert!(
                    fixture.voice().play_file(&replacement.0).await.is_err(),
                    "a new Play cannot override observed moderator restrictions"
                );
            }

            // Release moderation and any withheld self-state acknowledgement.
            // Neither may resurrect a decoder or the cancelled pending source.
            let mut released = initial;
            released.publication_revision = restricted_member.publication_revision;
            released.muted = true;
            released.deafened = true;
            fixture.hold_unmute.store(false, Ordering::Release);
            fixture
                .signals
                .send(ServerEvent::Participants {
                    members: vec![released],
                })
                .await
                .unwrap();
            fixture.silence().await;
            if matches!(phase, PlaybackPhase::PendingFence) {
                fixture.release_restriction.send(()).await.unwrap();
                fixture.silence().await;
            }
            assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Stopped);
            assert!(
                find_decoder(&replacement.0).is_none(),
                "cancelled decoder restarted"
            );
            assert!(!fixture._server.0.is_finished());

            // A genuinely new command still works, at the same publication
            // revision, and its own Stop must reap the fresh process.
            let (fresh_pid, _) = fixture.playing(&replacement).await;
            assert_ne!(fresh_pid, old_pid);
            fixture.voice().stop().await.unwrap();
            reaped(fresh_pid).await;
            fixture.silence().await;
            fixture.voice().leave().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("moderation lifecycle timed out: {phase:?}"));
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_moderation_stops_active_playback_until_new_play() {
    moderation_cancels_playback(PlaybackPhase::Active).await;
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_moderation_reaps_decoder_awaiting_unmute_until_new_play() {
    moderation_cancels_playback(PlaybackPhase::AwaitingUnmute).await;
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_moderation_cancels_pending_fence_until_new_play() {
    moderation_cancels_playback(PlaybackPhase::PendingFence).await;
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_moderation_at_admission_blocks_play_until_release() {
    timeout(Duration::from_secs(20), async {
        let wav = Wav::new(false);
        for server_deafened in [false, true] {
            let mut initial = member();
            initial.server_muted = !server_deafened;
            initial.server_deafened = server_deafened;
            initial.publication_revision = 12;
            let mut fixture = Fixture::join_with_member(initial.clone()).await;
            assert!(fixture.voice().play_file(&wav.0).await.is_err());
            fixture.silence().await;
            assert!(find_decoder(&wav.0).is_none());
            initial.server_muted = false;
            initial.server_deafened = false;
            initial.muted = true;
            initial.deafened = true;
            fixture
                .signals
                .send(ServerEvent::Participants {
                    members: vec![initial],
                })
                .await
                .unwrap();
            fixture.silence().await;
            assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Stopped);
            assert!(find_decoder(&wav.0).is_none());
            let (pid, _) = fixture.playing(&wav).await;
            fixture.voice().stop().await.unwrap();
            reaped(pid).await;
            fixture.voice().leave().await.unwrap();
        }
    })
    .await
    .expect("admission moderation lifecycle timed out");
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_active_stop_and_replacement_fence_rtp_and_reap_children() {
    timeout(Duration::from_secs(20), async {
        let first = Wav::new(false);
        let second = Wav::new(true);
        let mut fixture = Fixture::join().await;
        let (old_pid, old_packet) = fixture.playing(&first).await;
        let mut pcm = [0_i16; 1920];
        opus::Decoder::new(48_000, opus::Channels::Stereo)
            .unwrap()
            .decode(&old_packet.payload, &mut pcm, false)
            .unwrap();
        assert!(pcm.iter().any(|sample| sample.abs() > 100));

        fixture.hold_unmute.store(true, Ordering::Release);
        fixture.voice().play_file(&second.0).await.unwrap();
        reaped(old_pid).await;
        let new_pid = decoder_pid(&second.0).await;
        assert_ne!(old_pid, new_pid);
        fixture.silence().await;
        assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Starting);
        fixture.hold_unmute.store(false, Ordering::Release);
        fixture
            .signals
            .send(ServerEvent::Participants {
                members: vec![member()],
            })
            .await
            .unwrap();
        let new_packet = fixture.packet().await;
        assert!(new_packet.header.marker);
        assert!(
            new_packet
                .header
                .sequence_number
                .wrapping_sub(old_packet.header.sequence_number)
                < 100
        );
        let mut pcm = [0_i16; 1920];
        opus::Decoder::new(48_000, opus::Channels::Stereo)
            .unwrap()
            .decode(&new_packet.payload, &mut pcm, false)
            .unwrap();
        assert!(
            pcm.iter().all(|sample| sample.abs() <= 1),
            "old tone leaked into replacement silence"
        );

        timeout(Duration::from_secs(3), fixture.voice().stop())
            .await
            .unwrap()
            .unwrap();
        reaped(new_pid).await;
        assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Stopped);
        fixture.silence().await;
        fixture.voice().leave().await.unwrap();
    })
    .await
    .expect("active playback lifecycle timed out");
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_queued_rosters_cannot_admit_replacement_before_restriction_fence() {
    timeout(Duration::from_secs(20), async {
        let first = Wav::new(false);
        let second = Wav::new(true);
        let mut fixture = Fixture::join().await;
        let (old_pid, _) = fixture.playing(&first).await;
        fixture.queue_stale_rosters.store(true, Ordering::Release);
        fixture.voice().play_file(&second.0).await.unwrap();
        let mut queued = fixture.stale_rosters_sent.clone();
        timeout(Duration::from_secs(3), queued.wait_for(|value| *value))
            .await
            .unwrap()
            .unwrap();
        reaped(old_pid).await;
        fixture.silence().await;
        assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Starting);
        assert!(
            !fixture._server.0.is_finished(),
            "stale snapshots triggered premature unmute"
        );
        fixture.release_restriction.send(()).await.unwrap();
        let new_packet = fixture.packet().await;
        assert!(new_packet.header.marker);
        assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Playing);
        let new_pid = decoder_pid(&second.0).await;
        fixture.voice().stop().await.unwrap();
        reaped(new_pid).await;
        fixture.silence().await;
        fixture.voice().leave().await.unwrap();
    })
    .await
    .expect("queued-roster regression timed out");
}

#[tokio::test]
#[ignore = "requires Linux procfs and ffmpeg on PATH"]
async fn ffmpeg_active_revocation_and_final_drop_stop_rtp_and_reap_children() {
    timeout(Duration::from_secs(25), async {
        let wav = Wav::new(false);
        // Both the explicit revoke and the current permission snapshot must stop
        // an active decoder; neither can auto-resume its pre-revocation samples.
        for restriction in 0..3 {
            let mut fixture = Fixture::join().await;
            let (pid, _) = fixture.playing(&wav).await;
            let event = if restriction == 0 {
                ServerEvent::Revoked {}
            } else {
                let mut restricted = member();
                if restriction == 1 {
                    restricted.can_speak = false;
                } else {
                    restricted.muted = true;
                    restricted.deafened = true;
                }
                ServerEvent::Participants {
                    members: vec![restricted],
                }
            };
            fixture.signals.send(event).await.unwrap();
            let mut status = fixture.voice().status();
            timeout(
                Duration::from_secs(5),
                status.wait_for(|event| {
                    if restriction == 2 {
                        *event == PlaybackEvent::Stopped
                    } else {
                        matches!(event, PlaybackEvent::Error(_))
                    }
                }),
            )
            .await
            .unwrap()
            .unwrap();
            reaped(pid).await;
            fixture.silence().await;
            if restriction == 2 {
                // A later permissive roster must never auto-resume the old file.
                fixture
                    .signals
                    .send(ServerEvent::Participants {
                        members: vec![member()],
                    })
                    .await
                    .unwrap();
                fixture.silence().await;
                assert_eq!(*fixture.voice().status().borrow(), PlaybackEvent::Stopped);
                fixture.voice().leave().await.unwrap();
            } else {
                assert!(fixture.voice().play_file(&wav.0).await.is_err());
            }
        }
        let mut fixture = Fixture::join().await;
        let (pid, _) = fixture.playing(&wav).await;
        let mut status = fixture.voice().status();
        drop(fixture.voice.take());
        timeout(
            Duration::from_secs(5),
            status.wait_for(|event| *event == PlaybackEvent::Left),
        )
        .await
        .unwrap()
        .unwrap();
        reaped(pid).await;
        fixture.silence().await;
    })
    .await
    .expect("revocation/drop lifecycle timed out");
}
