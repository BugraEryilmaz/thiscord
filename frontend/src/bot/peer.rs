use super::*;
use crate::audio::{AudioEngine, Command, connection::Connection, outgoing, quality::Count, red};
use std::collections::VecDeque;

const PERIOD: Duration = Duration::from_millis(20);

struct Handler {
    gathered: Arc<Notify>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    incoming: mpsc::Sender<rtp::Packet>,
    video: mpsc::Sender<(rtp::Packet, Instant)>,
    stop: watch::Receiver<bool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            std::sync::atomic::Ordering::Release,
        );
    }
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let incoming = self.incoming.clone();
        let video = self.video.clone();
        let mut stop = self.stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    event = track.poll() => match event {
                        Some(TrackRemoteEvent::OnRtpPacket(packet)) => {
                            if packet.payload.len() <= 1500 && let Some((kind, _)) = MediaKind::from_relay_ssrc(packet.header.ssrc) {
                                if kind.is_video() {
                                    let _ = video.try_send((packet, Instant::now()));
                                } else {
                                    let _ = incoming.try_send(packet);
                                }
                            }
                        },
                        None => break,
                        _ => {},
                    }
                }
            }
        });
    }
}
struct Peer(Arc<dyn PeerConnection>);
impl Drop for Peer {
    fn drop(&mut self) {
        let pc = self.0.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), pc.close()).await;
        });
    }
}
async fn send(tx: &mpsc::Sender<String>, event: ClientEvent) -> Result<(), Failure> {
    let frame = serde_json::to_string(&ClientFrame {
        version: VOICE_VERSION,
        event,
    })
    .map_err(|_| Failure::Unavailable)?;
    tx.send(frame).await.map_err(|_| Failure::Unavailable)
}
async fn event(rx: &mut mpsc::Receiver<String>) -> Result<ServerEvent, Failure> {
    let Some(text) = rx.recv().await else {
        return Err(Failure::Unavailable);
    };
    serde_json::from_str::<ServerFrame>(&text)
        .map(|f| f.event)
        .map_err(|_| Failure::Unavailable)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    tx: mpsc::Sender<String>,
    mut rx: mpsc::Receiver<String>,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    mut settings: watch::Receiver<Settings>,
    mut stop: watch::Receiver<bool>,
    bot: Arc<StdMutex<Bot>>,
    ready: tokio::sync::oneshot::Sender<Result<(), Failure>>,
) {
    let (tracks_stop, tracks_rx) = watch::channel(false);
    let mut ready = Some(ready);
    let work = async {
        send(
            &tx,
            ClientEvent::Join {
                token,
                guild_id: guild,
                channel_id: channel,
                attempt: None,
            },
        )
        .await?;
        let (sdp, servers, packet_redundancy) = match event(&mut rx).await? {
            ServerEvent::Offer {
                sdp,
                ice_servers,
                can_speak: true,
                packet_redundancy,
                ..
            } => (sdp, ice_servers, packet_redundancy),
            ServerEvent::Error { error }
                if error.code == thiscord_shared::ErrorCode::RateLimited =>
            {
                return Err(Failure::Limited);
            }
            _ => return Err(Failure::Forbidden),
        };
        if !packet_redundancy {
            return Err(Failure::Unavailable);
        }
        let engine = AudioEngine::new();
        let connection = Connection::default();
        struct Call(AudioEngine, Connection);
        impl Drop for Call {
            fn drop(&mut self) {
                self.1.close();
                self.0.notify(Command::Stop);
            }
        }
        let _call = Call(engine.clone(), connection.clone());
        let transmit_control = engine.outgoing_control();
        transmit_control.enable_receive_protection();
        let gathered = Arc::new(Notify::new());
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (incoming_tx, mut incoming) = mpsc::channel(64);
        let (video_tx, mut video_incoming) = mpsc::channel(1024);
        let mut media = MediaEngine::default();
        media
            .register_codec(
                RTCRtpCodecParameters {
                    rtp_codec: RTCRtpCodec {
                        mime_type: "video/H264".into(),
                        clock_rate: 90_000,
                        sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                        ..Default::default()
                    },
                    payload_type: MediaKind::ScreenVideo.payload_type(),
                },
                RtpCodecKind::Video,
            )
            .map_err(|_| Failure::Unavailable)?;
        media
            .register_default_codecs()
            .map_err(|_| Failure::Unavailable)?;
        red::register(&mut media)?;
        let interceptors = register_default_interceptors(Registry::new(), &mut media)
            .map_err(|_| Failure::Unavailable)?;
        let interceptors = audio::adaptive_fec::register(interceptors, connection.clone());
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(configuration(&servers))
                .with_runtime(Arc::new(audio::media_runtime::MediaRuntime::default()))
                .with_media_engine(media)
                .with_interceptor_registry(interceptors)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    connected: connected.clone(),
                    incoming: incoming_tx,
                    video: video_tx,
                    stop: tracks_rx,
                }))
                .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
                .build()
                .await
                .map_err(|_| Failure::Unavailable)?,
        );
        let _peer = Peer(pc.clone());
        pc.set_remote_description(serde_json::from_str(&sdp).map_err(|_| Failure::Unavailable)?)
            .await
            .map_err(|_| Failure::Unavailable)?;
        let track = audio::transport::track(MediaKind::Microphone.publisher_ssrc());
        pc.add_track(track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| Failure::Unavailable)?;
        pc.set_local_description(
            pc.create_answer(None)
                .await
                .map_err(|_| Failure::Unavailable)?,
        )
        .await
        .map_err(|_| Failure::Unavailable)?;
        gathered.notified().await;
        send(
            &tx,
            ClientEvent::Answer {
                sdp: serde_json::to_string(
                    &pc.local_description().await.ok_or(Failure::Unavailable)?,
                )
                .map_err(|_| Failure::Unavailable)?,
                packet_redundancy: true,
                remote_audio: true,
                screen_feedback: true,
                screen_subscriptions: true,
                screen_layers: false,
                // Bots expose per-stream video diagnostics in their own panel.
                video_diagnostics: false,
            },
        )
        .await?;
        // Admission is complete only after the server accepts the answer.
        let mut members = loop {
            match event(&mut rx).await? {
                ServerEvent::Participants { members } => break members,
                ServerEvent::Error { .. } | ServerEvent::Revoked {} => {
                    return Err(Failure::Forbidden);
                }
                _ => {}
            }
        };
        let mut current = settings.borrow_and_update().clone();
        send(
            &tx,
            ClientEvent::State {
                muted: current.muted,
                deafened: current.deafened,
            },
        )
        .await?;
        let (frames, mut encoded) = mpsc::channel(8);
        let e = engine.clone();
        let c = connection.clone();
        let audio = current.audio.clone();
        tokio::task::spawn_blocking(move || {
            e.command(Command::VoiceStart {
                volumes: audio::volumes::Store::default().profile(guild),
                connection: c,
                settings: audio,
                outgoing: frames,
                microphone: true,
            })
        })
        .await
        .map_err(|_| Failure::Unavailable)??;
        engine.notify(Command::Pressed(current.talking));
        let mut rtp_clock = outgoing::RtpClock::default();
        let mut boundary = red::boundary::Boundary::default();
        let mut red_sender = red::Sender::default();
        let mut remote_jobs = tokio::task::JoinSet::new();
        let mut reply: Option<(thiscord_shared::RequestId, VecDeque<String>)> = None;
        let mut tick = tokio::time::interval(PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut uplink = Network::new(17);
        let mut downlink = Network::new(91);
        let mut video_link = Network::new(211);
        let mut video = super::video::Receiver::new();
        let id = bot.lock().unwrap().id;
        video.update(&members, id, &current);
        engine.notify_control(Command::Roster {
            connection: connection.clone(),
            members: members
                .iter()
                .filter(|m| m.account_id != id)
                .cloned()
                .collect(),
        })?;
        let mut roster = red::RosterCompatibility::default();
        roster.observe(&members).map_err(|error| error.message)?;
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        let mut ticks = 0u32;
        let result = loop {
            if !connection.active() {
                break Err(Failure::Unavailable);
            }
            let generation = transmit_control.generation();
            if boundary.control(generation)? {
                red_sender.clear();
                uplink.clear();
                downlink.clear();
                engine.notify_control(Command::ReceiveBoundary {
                    connection: connection.clone(),
                    generation,
                    epoch: None,
                })?;
            }
            if boundary.needs_refresh()
                && let Some(generation) = transmit_control.refresh_generation(generation)
            {
                boundary.start_refresh(generation)?;
                red_sender.clear();
                uplink.clear();
                downlink.clear();
                engine.notify_control(Command::ReceiveBoundary {
                    connection: connection.clone(),
                    generation,
                    epoch: None,
                })?;
            }
            if let Some((generation, challenge)) = boundary
                .request(Instant::now())
                .map_err(|_| Failure::Unavailable)?
            {
                send(
                    &tx,
                    match challenge {
                        Some(challenge) => ClientEvent::MicrophoneRefresh {
                            generation,
                            challenge,
                        },
                        None => ClientEvent::MicrophoneBoundary { generation },
                    },
                )
                .await?;
            }
            tokio::select! { biased;
                changed = settings.changed() => {
                    if changed.is_err() { break Ok(()); }
                    let audio = bot.lock().unwrap().settings.audio.clone();
                    current = settings.borrow_and_update().clone();
                    current.audio = audio;
                    bot.lock().unwrap().settings = current.clone();
                    audio::virtual_devices::update(&current);
                    engine.notify(Command::Pressed(current.talking));
                    uplink.clear(); downlink.clear();
                    if video.update(&members, id, &current) { video_link.clear(); }

                },
                server = event(&mut rx) => match server {
                    Ok(ServerEvent::Participants { members: next }) => {
                        members = next;
                        roster.observe(&members).map_err(|error| error.message)?;
                        engine.notify_control(Command::Roster { connection: connection.clone(), members: members.iter().filter(|m| m.account_id != id).cloned().collect() })?;
                        if video.update(&members, id, &current) { video_link.clear(); }
                    },
                    Ok(ServerEvent::MicrophoneRefreshRequired { challenge }) => boundary.require_refresh(challenge),
                    Ok(ServerEvent::MicrophoneBoundaryAccepted { generation, microphone_epoch, source_epoch, challenge, accepted }) => {
                        let control = transmit_control.generation();
                        if boundary.acknowledge(generation, challenge, accepted) && boundary.ready_control(control) && microphone_epoch != 0 && source_epoch != 0 {
                            engine.notify_control(Command::ReceiveBoundary { connection: connection.clone(), generation: control, epoch: Some(microphone_epoch) })?;
                            if let Some((generation, challenge)) = boundary.ready_notice() { send(&tx, ClientEvent::MicrophoneRefreshReady { generation, challenge }).await?; }
                        }
                    },
                    Ok(ServerEvent::AudioCommand { request_id, command }) => {
                        if remote_jobs.is_empty() && reply.is_none() {
                            let e = engine.clone(); let b = bot.clone(); let c = connection.clone();
                            remote_jobs.spawn_blocking(move || (request_id, super::control::execute(e, c, b, command)));
                        }
                    },
                    Ok(ServerEvent::Error { .. } | ServerEvent::Revoked {} | ServerEvent::Replaced {}) | Err(_) => break Err(Failure::Forbidden),
                    _ => {},
                },
                Some(result) = remote_jobs.join_next(), if !remote_jobs.is_empty() => {
                    let (id, result) = result.map_err(|_| Failure::Unavailable)?;
                    reply = Some((id, super::control::encode(result)));
                    current.audio = bot.lock().unwrap().settings.audio.clone();
                    if video.update(&members, id_account(&bot), &current) { video_link.clear(); }
                },
                _ = transmit_control.changed() => {},
                _ = tick.tick() => {
                    let now = Instant::now();
                    ticks += 1;
                    if let Some((request_id, chunks)) = &mut reply {
                        if let Some(chunk) = chunks.pop_front() { send(&tx, ClientEvent::AudioReply { request_id: *request_id, chunk, done: chunks.is_empty() }).await?; }
                        if chunks.is_empty() { reply = None; }
                    }
                    if ticks % 25 == 1 {
                        engine.notify(Command::Keepalive(connection.clone()));
                        if current.muted || current.deafened || boundary.ready() {
                            send(&tx, ClientEvent::State { muted: current.muted, deafened: current.deafened }).await?;
                        }
                        bot.lock().unwrap().output_peak = audio::virtual_devices::peak();
                        let (views, requests) = video.feedback(&bot);
                        if send(&tx, ClientEvent::ScreenViews { views }).await.is_err() { break Err(Failure::Unavailable); }
                        for request in requests { send(&tx, request).await?; }
                    }
                    while let Some(packet) = video_link.pop(now) {
                        if settings.has_changed().unwrap_or(true) { break; }
                        video.push(packet);
                    }
                    if ticks.is_multiple_of(250) && send(&tx, ClientEvent::Ping {}).await.is_err() { break Err(Failure::Unavailable); }
                    while let Some(packet) = uplink.pop(now) {
                        if settings.has_changed().unwrap_or(true) { break; }
                        if transmit_control.generation() != generation || !boundary.ready() { uplink.clear(); break; }
                        let sent = tokio::select! { biased;
                            _ = transmit_control.changed() => { uplink.clear(); break; },
                            result = tokio::time::timeout(PERIOD, track.write_rtp(packet)) => result,
                        };
                        match sent {
                            Ok(Ok(_)) => bot.lock().unwrap().sent_packets += 1,
                            _ => bot.lock().unwrap().dropped_packets += 1,
                        }
                    }
                    while let Some(packet) = downlink.pop(now) {
                        if settings.has_changed().unwrap_or(true) { break; }
                        receive(&engine, &connection, packet, &bot);
                    }
                },
                outgoing = encoded.recv() => {
                    let Some(mut frame) = outgoing else { break Err(Failure::Unavailable); };
                    if !connected.load(std::sync::atomic::Ordering::Acquire) { frame.finish(Count::OutgoingFailedPackets); continue; }
                    if let Err(reason) = transmit_control.check(&frame, &connection, Instant::now()) { red_sender.clear(); transmit_control.dropped_frame(reason, &mut frame); continue; }
                    if boundary.frame(&frame)? {
                        transmit_control.invalidate(); red_sender.clear(); uplink.clear(); downlink.clear();
                        engine.notify_control(Command::ReceiveBoundary { connection: connection.clone(), generation: transmit_control.generation(), epoch: None })?;
                    }
                    if !boundary.ready() { transmit_control.dropped_frame(outgoing::DropReason::ControlInvalidated, &mut frame); continue; }
                    if !red_sender.within_budget(&frame) { frame.finish(Count::OutgoingFailedPackets); continue; }
                    let (sequence, timestamp) = rtp_clock.pending(frame.media_position);
                    let enabled = frame.packet_redundancy;
                    let (payload_type, payload) = red_sender.packet(&frame, enabled, Instant::now());
                    let packet = rtp::Packet { header: rtp::header::Header { version: 2, payload_type, ssrc: MediaKind::Microphone.publisher_ssrc(), sequence_number: sequence, timestamp, marker: frame.marker, csrc: boundary.csrc(), ..Default::default() }, payload };
                    // The simulated link is after codec admission: lost frames remain in RED history,
                    // and receiver reports see real RTP sequence gaps, just as on a lossy network.
                    match transmit_control.guard_write_until(&frame, &connection, red_sender.deadline(&frame), async { uplink.push(packet, current.profile, Instant::now()) }).await {
                        Ok(accepted) => {
                            if !accepted { bot.lock().unwrap().dropped_packets += 1; }
                            frame.finish(Count::OutgoingSentPackets);
                            connection.uplink_sent(0, sequence); rtp_clock.next(frame.media_position);
                            red_sender.admitted(frame, enabled);
                        },
                        Err(reason) => { red_sender.clear(); transmit_control.dropped_frame(reason, &mut frame); }
                    }
                },
                Some(packet) = incoming.recv() => {
                    if !current.deafened { downlink.push(packet, current.profile, Instant::now()); }
                },
                Some((packet, received)) = video_incoming.recv() => {
                    if !current.deafened {
                        let accepted = received.elapsed() <= super::video::MAX_AGE
                            && video_link.push_video(packet.clone(), current.profile, Instant::now());
                        video.received(&packet, !accepted);
                    }
                }
            }
        };
        connection.close();
        engine.notify(Command::Stop);
        result
    };
    let result = tokio::select! {
        result = work => result,
        _ = stop.wait_for(|stop| *stop) => Ok(()),
        _ = tokio::time::sleep(Duration::from_secs(LIFETIME_SECS)) => Ok(()),
    };
    if let Some(ready) = ready {
        let _ = ready.send(result);
    }
    tracks_stop.send_replace(true);
    // Dropping the in-process signaling sender drives the SFU's normal cleanup.
}

/// A bounded media-link simulator. No sleeping on a codec or WebRTC callback.
struct Network {
    pending: VecDeque<(Instant, rtp::Packet)>,
    random: u32,
    credit: f64,
    last: Instant,
}
impl Network {
    fn new(seed: u32) -> Self {
        Self {
            pending: VecDeque::new(),
            random: seed,
            credit: 0.0,
            last: Instant::now(),
        }
    }
    fn clear(&mut self) {
        self.pending.clear();
        self.credit = 0.0;
        self.last = Instant::now();
    }
    fn push(&mut self, packet: rtp::Packet, profile: ConnectionProfile, now: Instant) -> bool {
        self.push_with(packet, profile, now, profile.parameters().3, 20)
    }
    fn push_video(
        &mut self,
        packet: rtp::Packet,
        profile: ConnectionProfile,
        now: Instant,
    ) -> bool {
        self.push_with(packet, profile, now, profile.video_bits(), 512)
    }
    fn push_with(
        &mut self,
        packet: rtp::Packet,
        profile: ConnectionProfile,
        now: Instant,
        bits: u32,
        capacity: usize,
    ) -> bool {
        let (delay, jitter, loss, _) = profile.parameters();
        self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
        let rate = bits as f64 / 8.0;
        self.credit =
            (self.credit + now.duration_since(self.last).as_secs_f64() * rate).min(rate * 0.1);
        self.last = now;
        let bytes = (packet.payload.len() + 60) as f64;
        if self.random % 100 < loss || self.credit < bytes || self.pending.len() >= capacity {
            return false;
        }
        self.credit -= bytes;
        let due = now + Duration::from_millis(delay + (u64::from(self.random >> 8) % (jitter + 1)));
        let index = self
            .pending
            .iter()
            .position(|(at, _)| *at > due)
            .unwrap_or(self.pending.len());
        self.pending.insert(index, (due, packet));
        true
    }
    fn pop(&mut self, now: Instant) -> Option<rtp::Packet> {
        self.pending
            .front()
            .is_some_and(|(at, _)| *at <= now)
            .then(|| self.pending.pop_front().unwrap().1)
    }
}

fn id_account(bot: &Arc<StdMutex<Bot>>) -> AccountId {
    bot.lock().unwrap().id
}
fn receive(
    engine: &AudioEngine,
    connection: &Connection,
    packet: rtp::Packet,
    bot: &Arc<StdMutex<Bot>>,
) {
    let Some((kind, slot)) = MediaKind::from_relay_ssrc(packet.header.ssrc) else {
        return;
    };
    let Some(slot) = kind.mixer_slot(slot) else {
        return;
    };
    if slot < ROOM_CAPACITY && !matches!(packet.header.csrc.len(), 0 | 2) {
        return;
    }
    engine.notify(Command::Packet {
        payload_type: packet.header.payload_type,
        source_epoch: (slot < ROOM_CAPACITY)
            .then(|| packet.header.csrc.first().copied())
            .flatten(),
        privacy_epoch: (slot < ROOM_CAPACITY && packet.header.csrc.len() == 2)
            .then(|| packet.header.csrc[1]),
        marker: packet.header.marker,
        connection: connection.clone(),
        slot,
        sequence: packet.header.sequence_number,
        timestamp: packet.header.timestamp,
        received_at: Instant::now(),
        payload: packet.payload,
    });
    let mut bot = bot.lock().unwrap();
    bot.received_packets += 1;
    bot.received_system_audio_packets += u64::from(kind == MediaKind::SystemAudio);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn loopback_companion_gathers_without_waiting_for_public_stun_or_turn() {
        // A reachable socket that never answers models coturn rejecting a
        // loopback peer on the same server. An unroutable IP fails immediately
        // and would not reproduce the production gathering timeout.
        let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = blackhole.local_addr().unwrap();
        let gathered = Arc::new(Notify::new());
        let (incoming, _) = mpsc::channel(1);
        let (video, _) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        let pc = PeerConnectionBuilder::new()
            .with_configuration(configuration(&[
                IceServer {
                    urls: vec![format!("stun:{address}")],
                    username: String::new(),
                    credential: String::new(),
                },
                IceServer {
                    urls: vec![format!("turn:{address}?transport=udp")],
                    username: "synthetic".into(),
                    credential: "synthetic".into(),
                },
            ]))
            .with_media_engine(media)
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                connected: Arc::new(false.into()),
                incoming,
                video,
                stop,
            }))
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .build()
            .await
            .unwrap();
        pc.add_track(audio::transport::track(
            MediaKind::Microphone.publisher_ssrc(),
        ))
        .await
        .unwrap();
        pc.set_local_description(pc.create_offer(None).await.unwrap())
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(3), gathered.notified()).await;
        pc.close().await.unwrap();
        assert!(
            completed.is_ok(),
            "local bots must not wait for external ICE gathering"
        );
    }
    #[test]
    fn profiles_drop_packets_and_bound_delayed_work() {
        for profile in ConnectionProfile::ALL {
            let mut network = Network::new(17);
            let start = network.last;
            let mut delivered = 0;
            for frame in 1..=1000 {
                let now = start + PERIOD * frame;
                network.push(
                    rtp::Packet {
                        payload: vec![0; 80].into(),
                        ..Default::default()
                    },
                    profile,
                    now,
                );
                while network.pop(now).is_some() {
                    delivered += 1;
                }
                assert!(network.pending.len() <= 20);
            }
            match profile {
                ConnectionProfile::Good => assert!(delivered > 990),
                _ => assert!(delivered < 950),
            }
            network.clear();
            assert!(network.pending.is_empty());
        }
    }
}
