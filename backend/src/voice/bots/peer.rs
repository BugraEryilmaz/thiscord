use super::*;
use rtc::rtp_transceiver::rtp_sender::*;
use std::collections::VecDeque;

const FRAME: usize = 960;
const PERIOD: Duration = Duration::from_millis(20);

struct Handler {
    gathered: Arc<Notify>,
    incoming: mpsc::Sender<rtp::Packet>,
    stop: watch::Receiver<bool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let incoming = self.incoming.clone();
        let mut stop = self.stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    event = track.poll() => match event {
                        Some(TrackRemoteEvent::OnRtpPacket(packet)) => { let _ = incoming.try_send(packet); },
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
async fn send(
    tx: &mpsc::Sender<axum::extract::ws::Message>,
    event: ClientEvent,
) -> Result<(), Failure> {
    let frame = serde_json::to_string(&ClientFrame {
        version: VOICE_VERSION,
        event,
    })
    .map_err(|_| Failure::Unavailable)?;
    tx.send(axum::extract::ws::Message::Text(frame.into()))
        .await
        .map_err(|_| Failure::Unavailable)
}
async fn event(
    rx: &mut mpsc::Receiver<axum::extract::ws::Message>,
) -> Result<ServerEvent, Failure> {
    let Some(axum::extract::ws::Message::Text(text)) = rx.recv().await else {
        return Err(Failure::Unavailable);
    };
    serde_json::from_str::<ServerFrame>(&text)
        .map(|f| f.event)
        .map_err(|_| Failure::Unavailable)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    tx: mpsc::Sender<axum::extract::ws::Message>,
    mut rx: mpsc::Receiver<axum::extract::ws::Message>,
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
        let (sdp, servers) = match event(&mut rx).await? {
            ServerEvent::Offer {
                sdp,
                ice_servers,
                can_speak: true,
                ..
            } => (sdp, ice_servers),
            ServerEvent::Error { error }
                if error.code == thiscord_shared::ErrorCode::RateLimited =>
            {
                return Err(Failure::Limited);
            }
            _ => return Err(Failure::Forbidden),
        };
        let gathered = Arc::new(Notify::new());
        let (incoming_tx, mut incoming) = mpsc::channel(64);
        let mut media = MediaEngine::default();
        media
            .register_default_codecs()
            .map_err(|_| Failure::Unavailable)?;
        let interceptors = register_default_interceptors(Registry::new(), &mut media)
            .map_err(|_| Failure::Unavailable)?;
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(ice::configuration(&servers))
                .with_media_engine(media)
                .with_interceptor_registry(interceptors)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    incoming: incoming_tx,
                    stop: tracks_rx,
                }))
                .with_udp_addrs(vec![local_bind().unwrap_or_else(|_| "127.0.0.1:0".into())])
                .build()
                .await
                .map_err(|_| Failure::Unavailable)?,
        );
        let _peer = Peer(pc.clone());
        pc.set_remote_description(serde_json::from_str(&sdp).map_err(|_| Failure::Unavailable)?)
            .await
            .map_err(|_| Failure::Unavailable)?;
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            "test-bot".into(),
            "microphone".into(),
            "test-bot".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(900),
                    ..Default::default()
                },
                codec: RTCRtpCodec {
                    mime_type: "audio/opus".into(),
                    clock_rate: 48_000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
        )));
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
                packet_redundancy: false,
                remote_audio: false,
                screen_feedback: false,
                screen_subscriptions: true,
                screen_layers: false,
            },
        )
        .await?;
        // Admission is complete only after the server accepts the answer.
        loop {
            match event(&mut rx).await? {
                ServerEvent::Participants { .. } => break,
                ServerEvent::Error { .. } | ServerEvent::Revoked {} => {
                    return Err(Failure::Forbidden);
                }
                _ => {}
            }
        }
        let mut current = settings.borrow_and_update().clone();
        send(
            &tx,
            ClientEvent::State {
                muted: current.muted,
                deafened: current.deafened,
            },
        )
        .await?;
        let (jobs, mut encoded, worker) = codec(bot.clone());
        if let Some(ready) = ready.take() {
            let _ = ready.send(Ok(()));
        }
        let mut tick = tokio::time::interval(PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let started = Instant::now();
        let mut sequence = 0u16;
        let mut epoch = 0u64;
        let mut uplink = Network::new(17);
        let mut downlink = Network::new(91);
        let mut ticks = 0u32;
        let result = loop {
            tokio::select! { biased;
                changed = settings.changed() => {
                    if changed.is_err() { break Ok(()); }
                    current = settings.borrow_and_update().clone();
                    epoch = epoch.wrapping_add(1);
                    uplink.clear(); downlink.clear();
                    if send(&tx, ClientEvent::State { muted: current.muted, deafened: current.deafened }).await.is_err() { break Err(Failure::Unavailable); }
                },
                server = event(&mut rx) => match server {
                    Ok(ServerEvent::Error { .. } | ServerEvent::Revoked {} | ServerEvent::Replaced {}) | Err(_) => break Err(Failure::Forbidden),
                    _ => {},
                },
                outgoing = encoded.recv() => {
                    let Some((generation, packet)) = outgoing else { break Err(Failure::Unavailable); };
                    if epoch == generation && !current.muted && !current.deafened && !uplink.push(packet, current.profile, Instant::now()) {
                        bot.lock().unwrap().dropped_packets += 1;
                    }
                },
                Some(packet) = incoming.recv() => {
                    if !current.deafened { downlink.push(packet, current.profile, Instant::now()); }
                },
                _ = tick.tick() => {
                    let now = Instant::now();
                    ticks += 1;
                    if ticks.is_multiple_of(250) && send(&tx, ClientEvent::Ping {}).await.is_err() { break Err(Failure::Unavailable); }
                    sequence = sequence.wrapping_add(1);
                    let sample = (started.elapsed().as_secs_f64() * 48_000.0) as u64;
                    if !current.muted && !current.deafened {
                        let _ = jobs.try_send(Job::Encode { epoch, sequence, sample, settings: current.clone() });
                    }
                    while let Some(packet) = uplink.pop(now) {
                        if settings.has_changed().unwrap_or(true) { break; }
                        match tokio::time::timeout(PERIOD, track.write_rtp(packet)).await {
                            Ok(Ok(_)) => bot.lock().unwrap().sent_packets += 1,
                            _ => bot.lock().unwrap().dropped_packets += 1,
                        }
                    }
                    while let Some(packet) = downlink.pop(now) {
                        if settings.has_changed().unwrap_or(true) { break; }
                        let _ = jobs.try_send(Job::Decode(packet, current.output_volume));
                    }
                }
            }
        };
        drop(jobs);
        let _ = worker.await;
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
        let (delay, jitter, loss, bits) = profile.parameters();
        self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
        let rate = bits as f64 / 8.0;
        self.credit =
            (self.credit + now.duration_since(self.last).as_secs_f64() * rate).min(rate * 0.1);
        self.last = now;
        let bytes = (packet.payload.len() + 60) as f64;
        if self.random % 100 < loss || self.credit < bytes || self.pending.len() >= 20 {
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

enum Job {
    Encode {
        epoch: u64,
        sequence: u16,
        sample: u64,
        settings: Settings,
    },
    Decode(rtp::Packet, f32),
}
type Encoded = (u64, rtp::Packet);
fn codec(
    bot: Arc<StdMutex<Bot>>,
) -> (
    mpsc::Sender<Job>,
    mpsc::Receiver<Encoded>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut jobs) = mpsc::channel(16);
    let (encoded, rx) = mpsc::channel(8);
    let worker = tokio::task::spawn_blocking(move || {
        let Ok(mut encoder) =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip)
        else {
            return;
        };
        let mut decoders = HashMap::new();
        let mut encoder_epoch = None;
        let mut pcm = [0.0f32; FRAME];
        let mut bytes = [0u8; 1500];
        let mut decoded = [0.0f32; 5760];
        while let Some(job) = jobs.blocking_recv() {
            match job {
                Job::Encode {
                    epoch,
                    sequence,
                    sample,
                    settings,
                } => {
                    if encoder_epoch != Some(epoch) {
                        let _ = encoder.reset_state();
                        encoder_epoch = Some(epoch);
                    }
                    let bitrate = if settings.profile == ConnectionProfile::LowBandwidth {
                        16_000
                    } else {
                        32_000
                    };
                    let _ = encoder.set_bitrate(opus::Bitrate::Bits(bitrate));
                    let _ = encoder.set_inband_fec(true);
                    let _ = encoder.set_packet_loss_perc(settings.profile.parameters().2 as i32);
                    for (index, value) in pcm.iter_mut().enumerate() {
                        *value = (loop_sample(settings.microphone_loop, sample + index as u64)
                            * settings.input_gain)
                            .clamp(-0.95, 0.95);
                    }
                    if let Ok(size) = encoder.encode_float(&pcm, &mut bytes) {
                        let packet = rtp::Packet {
                            header: rtp::header::Header {
                                version: 2,
                                payload_type: 111,
                                ssrc: 900,
                                sequence_number: sequence,
                                timestamp: sample as u32,
                                ..Default::default()
                            },
                            payload: bytes[..size].to_vec().into(),
                        };
                        let _ = encoded.try_send((epoch, packet));
                    }
                }
                Job::Decode(packet, gain) => {
                    if bot.lock().unwrap().settings.deafened {
                        decoders.clear();
                        continue;
                    }
                    if !decoders.contains_key(&packet.header.ssrc)
                        && decoders.len() < ROOM_CAPACITY * 2
                        && let Ok(decoder) = opus::Decoder::new(48_000, opus::Channels::Mono)
                    {
                        decoders.insert(packet.header.ssrc, decoder);
                    }
                    if let Some(decoder) = decoders.get_mut(&packet.header.ssrc)
                        && let Ok(size) = decoder.decode_float(&packet.payload, &mut decoded, false)
                    {
                        let mut bot = bot.lock().unwrap();
                        bot.received_packets += 1;
                        bot.output_peak = decoded[..size]
                            .iter()
                            .map(|v| (v * gain).abs().min(1.0))
                            .fold(0.0, f32::max);
                    }
                }
            }
        }
    });
    (tx, rx, worker)
}

/// Original synthesized three-second loops with quiet gaps; no recordings/assets.
fn loop_sample(kind: MicrophoneLoop, sample: u64) -> f32 {
    let time = (sample % 144_000) as f32 / 48_000.0;
    if time > 2.4 {
        return 0.0;
    }
    let note = (time * 4.0) as usize;
    let frequencies = match kind {
        MicrophoneLoop::Chimes => [523.25, 659.25, 783.99, 1046.5],
        MicrophoneLoop::Pulse => [130.81, 130.81, 196.0, 164.81],
        MicrophoneLoop::Melody => [392.0, 493.88, 587.33, 493.88],
    };
    let local = time % 0.25;
    let envelope = (local * 100.0).min(1.0) * (-local * 14.0).exp();
    (std::f32::consts::TAU * frequencies[note % 4] * local).sin() * envelope * 0.12
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loops_are_distinct_quiet_bounded_and_decodable() {
        let mut previous = Vec::new();
        for kind in MicrophoneLoop::ALL {
            let samples: Vec<_> = (0..FRAME as u64).map(|s| loop_sample(kind, s)).collect();
            assert_ne!(samples, previous);
            assert!(samples.iter().any(|s| s.abs() > 0.01));
            assert!(samples.iter().all(|s| s.abs() <= 0.12));
            assert_eq!(loop_sample(kind, 130_000), 0.0);
            let mut encoder =
                opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip).unwrap();
            let mut bytes = [0; 1500];
            let size = encoder.encode_float(&samples, &mut bytes).unwrap();
            let mut decoder = opus::Decoder::new(48_000, opus::Channels::Mono).unwrap();
            let mut decoded = [0.0; FRAME];
            assert_eq!(
                decoder
                    .decode_float(&bytes[..size], &mut decoded, false)
                    .unwrap(),
                FRAME
            );
            previous = samples;
        }
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
