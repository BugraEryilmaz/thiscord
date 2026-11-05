//! Headless remote SFU load generator. No audio devices, DB access or token files.
#[path = "voice_load/common.rs"]
mod common;
#[path = "voice_load/metrics.rs"]
mod metrics;

use common::{Args, Result};
use futures_util::{SinkExt, StreamExt};
use metrics::{Delivery, Stats, percentile};
use rtc::{media_stream::MediaStreamTrack, rtp, rtp_transceiver::rtp_sender::*};
use serde_json::json;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use thiscord_shared::{
    ChannelId, GuildId,
    voice::{self, ClientEvent, ClientFrame, ServerEvent, ServerFrame},
};
use tokio::sync::{Notify, mpsc, watch};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
use webrtc::{
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const PERIOD: Duration = Duration::from_millis(20);
const WARMUP: u32 = u32::MAX;
/// Synthetic camera frames per second; each frame is split into equal RTP packets.
const VIDEO_FPS: u32 = 30;
const VIDEO_PACKET_BYTES: usize = 1100;
/// Forced camera keyframe spacing, so new or recovering viewers resynchronize.
const KEYFRAME_FRAMES: u32 = VIDEO_FPS;
const MAGIC: &[u8; 8] = b"TCLDv001";

struct Config {
    endpoint: String,
    bind: String,
    secret: String,
    users: usize,
    room_size: usize,
    seconds: usize,
    bytes: usize,
    late_ms: u64,
    relay: bool,
    /// Camera publication bitrate (bits/s); 0 disables the video workload.
    camera_bitrate: usize,
    epoch: Instant,
    run: [u8; 8],
}
impl Config {
    fn video(&self) -> bool {
        self.camera_bitrate > 0
    }
    /// Equal RTP packets per synthetic camera frame (at least one).
    fn video_packets_per_frame(&self) -> usize {
        (self.camera_bitrate / 8 / VIDEO_FPS as usize)
            .div_ceil(VIDEO_PACKET_BYTES)
            .max(1)
    }
    fn video_packet_bytes(&self) -> usize {
        (self.camera_bitrate / 8 / VIDEO_FPS as usize / self.video_packets_per_frame())
            .clamp(40, VIDEO_PACKET_BYTES)
    }
    fn video_packets(&self) -> usize {
        self.seconds * VIDEO_FPS as usize * self.video_packets_per_frame()
    }
}

fn endpoint(base: &str, insecure: bool) -> Result<String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid --url")?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("--url must be a server origin without credentials, path, query or fragment");
    }
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" if insecure => "ws",
        _ => return Err("Use https, or explicitly pass --allow-insecure for a trusted LAN"),
    };
    url.set_scheme(scheme).map_err(|_| "Invalid URL scheme")?;
    url.set_path(voice::VOICE_PATH);
    Ok(url.to_string())
}

struct Handler {
    gathered: Arc<Notify>,
    config: Arc<Config>,
    index: usize,
    stats: Arc<Mutex<Stats>>,
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
        let config = self.config.clone();
        let stats = self.stats.clone();
        let index = self.index;
        let mut stop = self.stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    event = track.poll() => {
                        match event {
                            Some(TrackRemoteEvent::OnRtpPacket(packet)) => receive(&config, index, &mut stats.lock().unwrap(), &packet.payload),
                            None => break,
                            _ => {},
                        }
                    }
                }
            }
        });
    }
}

fn receive(config: &Config, index: usize, stats: &mut Stats, payload: &[u8]) {
    // Video payloads carry one H.264 NAL header byte before the measurement header.
    let video =
        config.video() && payload.len() == config.video_packet_bytes() && &payload[1..9] == MAGIC;
    let header = if video {
        &payload[1..]
    } else if payload.len() == config.bytes && &payload[..8] == MAGIC {
        payload
    } else {
        stats.invalid += 1;
        return;
    };
    if header[8..16] != config.run {
        stats.invalid += 1;
        return;
    }
    let source = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    let sequence = u32::from_be_bytes(header[20..24].try_into().unwrap());
    let sent_us = u64::from_be_bytes(header[24..32].try_into().unwrap());
    let now = config.epoch.elapsed().as_micros() as u64;
    if source >= config.users
        || source == index
        || source / config.room_size != index / config.room_size
        || sent_us > now
    {
        stats.invalid += 1;
        return;
    }
    let (stream, limit) = if video {
        (
            &mut stats.video_streams[source % config.room_size],
            config.video_packets(),
        )
    } else {
        (
            &mut stats.streams[source % config.room_size],
            config.seconds * 50,
        )
    };
    if sequence == WARMUP {
        stream.warm = true;
        return;
    }
    if sequence as usize >= limit {
        stats.invalid += 1;
        return;
    }
    stream.record(sequence as usize, (now - sent_us) / 1000, config.late_ms);
}

async fn send(socket: &mut Socket, event: ClientEvent) -> Result<()> {
    let frame = serde_json::to_string(&ClientFrame {
        version: voice::VOICE_VERSION,
        event,
    })
    .map_err(|_| "Cannot serialize signaling")?;
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(frame.into())),
    )
    .await
    .map_err(|_| "Signaling send timeout")?
    .map_err(|_| "Signaling send failed")
}
async fn event(socket: &mut Socket) -> Result<ServerEvent> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let frame: ServerFrame =
                    serde_json::from_str(&text).map_err(|_| "Invalid server signaling")?;
                if frame.version != voice::VOICE_VERSION {
                    return Err("Incompatible voice version");
                }
                return Ok(frame.event);
            }
            Some(Ok(Message::Ping(data))) => socket
                .send(Message::Pong(data))
                .await
                .map_err(|_| "WebSocket pong failed")?,
            Some(Ok(Message::Pong(_))) => {}
            _ => return Err("Signaling disconnected"),
        }
    }
}

struct Peer {
    socket: Socket,
    connection: Connection,
    track: Arc<TrackLocalStaticRTP>,
    camera: Option<Arc<TrackLocalStaticRTP>>,
}
struct Connection {
    pc: Arc<dyn PeerConnection>,
    stop: watch::Sender<bool>,
    closed: bool,
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if !self.closed {
            let pc = self.pc.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(3), pc.close()).await;
            });
        }
    }
}
impl Peer {
    async fn close(mut self) {
        let _ = self.connection.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(1), self.socket.close(None)).await;
        self.connection.closed =
            tokio::time::timeout(Duration::from_secs(3), self.connection.pc.close())
                .await
                .is_ok();
    }
}

async fn join(config: Arc<Config>, index: usize, stats: Arc<Mutex<Stats>>) -> Result<Peer> {
    let mut request = config
        .endpoint
        .as_str()
        .into_client_request()
        .map_err(|_| "Invalid WebSocket URL")?;
    request
        .headers_mut()
        .insert("origin", common::ORIGIN.parse().unwrap());
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(
            request,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(128 * 1024))
                    .max_frame_size(Some(128 * 1024)),
            ),
            false,
        ),
    )
    .await
    .map_err(|_| "WebSocket connection timeout")?
    .map_err(|_| "WebSocket connection failed (check address, TLS, firewall or server capacity)")?;
    send(
        &mut socket,
        ClientEvent::Join {
            token: common::token(&config.secret, index),
            guild_id: GuildId::from_uuid(common::id("guild", 0)),
            channel_id: ChannelId::from_uuid(common::id("channel", index / config.room_size)),
            attempt: None,
        },
    )
    .await?;
    let ServerEvent::Offer {
        sdp,
        can_speak: true,
        ice_servers,
        ..
    } = tokio::time::timeout(Duration::from_secs(20), event(&mut socket))
        .await
        .map_err(|_| "Offer timeout")??
    else {
        return Err("Join rejected: check matching secret, fixture size, room size and capacity");
    };
    let gathered = Arc::new(Notify::new());
    let (stop_tx, stop) = watch::channel(false);
    let mut media = MediaEngine::default();
    // Match the SFU's H.264 offer exactly, as the native clients and voice bots do.
    media
        .register_codec(
            RTCRtpCodecParameters {
                rtp_codec: RTCRtpCodec {
                    mime_type: "video/H264".into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                    ..Default::default()
                },
                payload_type: voice::MediaKind::Camera.payload_type(),
            },
            RtpCodecKind::Video,
        )
        .map_err(|_| "Media engine setup failed")?;
    media
        .register_default_codecs()
        .map_err(|_| "Media engine setup failed")?;
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(
            ice_servers
                .into_iter()
                .map(|s| RTCIceServer {
                    urls: s.urls,
                    username: s.username,
                    credential: s.credential,
                })
                .collect(),
        )
        .with_ice_transport_policy(if config.relay {
            RTCIceTransportPolicy::Relay
        } else {
            RTCIceTransportPolicy::All
        })
        .build();
    let registry = register_default_interceptors(Registry::new(), &mut media)
        .map_err(|_| "Media interceptors failed")?;
    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                config: config.clone(),
                index,
                stats,
                stop,
            }))
            .with_udp_addrs(vec![config.bind.clone()])
            .build()
            .await
            .map_err(|_| "Cannot bind WebRTC socket")?,
    );
    let connection = Connection {
        pc: pc.clone(),
        stop: stop_tx,
        closed: false,
    };
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        pc.set_remote_description(serde_json::from_str(&sdp).map_err(|_| "Invalid offer")?)
            .await
            .map_err(|_| "Cannot apply offer")?;
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            "load".into(),
            "microphone".into(),
            "load".into(),
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
            .map_err(|_| "Cannot add sender")?;
        let camera = if config.video() {
            let ssrc = voice::MediaKind::Camera.publisher_ssrc();
            let camera = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
                format!("camera-{ssrc}"),
                format!("camera-{ssrc}"),
                format!("camera-{ssrc}"),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: RTCRtpCodec {
                        mime_type: "video/H264".into(),
                        clock_rate: 90_000,
                        sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
            )));
            pc.add_track(camera.clone() as Arc<dyn TrackLocal>)
                .await
                .map_err(|_| "Cannot add camera sender")?;
            Some(camera)
        } else {
            None
        };
        pc.set_local_description(
            pc.create_answer(None)
                .await
                .map_err(|_| "Cannot create answer")?,
        )
        .await
        .map_err(|_| "Cannot apply answer")?;
        gathered.notified().await;
        send(
            &mut socket,
            ClientEvent::Answer {
                browser_publisher: false,
                packet_redundancy: false,
                remote_audio: false,
                // Camera views are leased subscriptions; feedback carries keyframe requests.
                screen_feedback: config.video(),
                screen_subscriptions: config.video(),
                screen_layers: false,
                screen_profiles: false,
                video_diagnostics: false,
                soundboard: false,
                sdp: serde_json::to_string(&pc.local_description().await.ok_or("Missing answer")?)
                    .map_err(|_| "Cannot serialize answer")?,
            },
        )
        .await?;
        if camera.is_some() {
            send(&mut socket, ClientEvent::Camera { active: true }).await?;
        }
        Ok((track, camera))
    })
    .await
    .unwrap_or(Err("ICE negotiation timed out"));
    match result {
        Ok((track, camera)) => Ok(Peer {
            socket,
            connection,
            track,
            camera,
        }),
        Err(e) => Err(e),
    }
}

/// Leased camera subscriptions for every other active camera in the room.
fn camera_views(
    members: &[voice::Participant],
    own: usize,
) -> Vec<thiscord_shared::screen::Subscription> {
    members
        .iter()
        .filter(|m| m.slot != own && m.camera_active)
        .map(|m| thiscord_shared::screen::Subscription {
            kind: voice::MediaKind::Camera,
            slot: m.slot,
            owner: m.account_id,
            epoch: m.camera_epoch,
            bitrate: thiscord_shared::screen::MAX_VIEW_BITRATE,
            h264_high: false,
        })
        .collect()
}

async fn exercise(
    peer: &mut Peer,
    config: &Config,
    index: usize,
    stats: &Mutex<Stats>,
    mut start: watch::Receiver<Option<Instant>>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut tick = tokio::time::interval(PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    let mut sequence = 0_u16;
    let mut timestamp = 0_u32;
    let mut last_measured = None;
    let mut last_pong = Instant::now();
    let video_period = Duration::from_secs(1) / VIDEO_FPS;
    let mut video_tick = tokio::time::interval(video_period);
    video_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut video_sequence = 0_u16;
    let mut video_frame = 0_u32;
    let mut last_video_frame = None;
    let mut keyframe = true;
    let mut views = Vec::new();
    let mut views_renewed = Instant::now();
    let own_account = thiscord_shared::AccountId::from_uuid(common::id("account", index));
    let ppf = config.video_packets_per_frame();
    loop {
        tokio::select! {
            _ = stop.changed() => return Err("Cancelled"),
            changed = start.changed() => {
                if changed.is_err() { return Err("Coordinator stopped"); }
                let (local, remote) = tokio::time::timeout(Duration::from_secs(1), selected_route(peer.connection.pc.as_ref())).await.map_err(|_| "Route inspection timeout")??;
                if config.relay && local != "relay" { return Err("Required relay route was not selected"); }
                let mut stats = stats.lock().unwrap();
                stats.local_candidate = Some(local);
                stats.remote_candidate = Some(remote);
                // Align this sender's timer to the common measurement grid. Leaving
                // it on its join-time phase could turn harmless sub-millisecond
                // jitter across a grid boundary into a skipped/duplicate send slot.
                if let Some(begin) = *start.borrow() {
                    tick.reset_at(tokio::time::Instant::from_std(begin));
                    video_tick.reset_at(tokio::time::Instant::from_std(begin));
                }
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(20) { return Err("Heartbeat timeout"); }
                send(&mut peer.socket, ClientEvent::Ping {}).await?;
            },
            incoming = event(&mut peer.socket) => match incoming? {
                ServerEvent::Pong {} => last_pong = Instant::now(),
                ServerEvent::Participants { members } => {
                    if peer.camera.is_some() {
                        let own = members.iter().find(|m| m.account_id == own_account).map(|m| m.slot);
                        let next = own.map(|s| camera_views(&members, s)).unwrap_or_default();
                        // Renew before the server's view lease expires, and on any change.
                        if next != views || views_renewed.elapsed() >= Duration::from_secs(1) {
                            views = next;
                            views_renewed = Instant::now();
                            send(&mut peer.socket, ClientEvent::ScreenViews { views: views.clone() }).await?;
                        }
                    }
                },
                ServerEvent::ScreenKeyframe { .. } => keyframe = true,
                ServerEvent::ScreenTarget { .. } | ServerEvent::MediaDiagnostics { .. } => {},
                _ => return Err("Server revoked or rejected voice session"),
            },
            _ = video_tick.tick(), if peer.camera.is_some() => {
                let now = Instant::now();
                let start_time = *start.borrow();
                let measured = start_time.and_then(|s| now.checked_duration_since(s));
                if let Some(elapsed) = measured && elapsed >= Duration::from_secs(config.seconds as u64) {
                    continue;
                }
                let frame = measured.map_or(WARMUP, |d| (d.as_micros() / video_period.as_micros()) as u32);
                if frame != WARMUP && last_video_frame == Some(frame) { continue; }
                video_frame = video_frame.wrapping_add(1);
                let idr = keyframe || video_frame.is_multiple_of(KEYFRAME_FRAMES);
                keyframe = false;
                let rtp_timestamp = video_frame.wrapping_mul(90_000 / VIDEO_FPS);
                let camera = peer.camera.clone().unwrap();
                let bytes = config.video_packet_bytes();
                for part in 0..ppf {
                    let number = if frame == WARMUP { WARMUP } else { frame * ppf as u32 + part as u32 };
                    let mut payload = vec![0_u8; bytes];
                    // Single NAL unit packets: IDR slices for keyframes, else non-IDR.
                    payload[0] = if idr { 0x65 } else { 0x41 };
                    payload[1..9].copy_from_slice(MAGIC);
                    payload[9..17].copy_from_slice(&config.run);
                    payload[17..21].copy_from_slice(&(index as u32).to_be_bytes());
                    payload[21..25].copy_from_slice(&number.to_be_bytes());
                    payload[25..33].copy_from_slice(&(config.epoch.elapsed().as_micros() as u64).to_be_bytes());
                    video_sequence = video_sequence.wrapping_add(1);
                    let packet = rtp::Packet { header: rtp::header::Header { version: 2, payload_type: voice::MediaKind::Camera.payload_type(), ssrc: voice::MediaKind::Camera.publisher_ssrc(), sequence_number: video_sequence, timestamp: rtp_timestamp, marker: part + 1 == ppf, ..Default::default() }, payload: payload.into() };
                    let result = tokio::time::timeout(Duration::from_millis(100), camera.write_rtp(packet)).await;
                    if frame != WARMUP {
                        let mut stats = stats.lock().unwrap();
                        stats.video_attempted += 1;
                        if matches!(result, Ok(Ok(_))) { stats.video_sent.insert(number as usize); } else { stats.send_errors += 1; }
                    }
                }
                if frame != WARMUP { last_video_frame = Some(frame); }
            },
            _ = tick.tick() => {
                let now = Instant::now();
                let start_time = *start.borrow();
                let measured = start_time.and_then(|s| now.checked_duration_since(s));
                if let Some(elapsed) = measured && elapsed >= Duration::from_secs(config.seconds as u64) {
                    if elapsed >= Duration::from_secs(config.seconds as u64 + 2) { return Ok(()); }
                    continue; // Bounded drain: count trailing loss, including entirely missing streams.
                }
                let number = measured.map_or(WARMUP, |d| (d.as_millis() / 20) as u32);
                if number != WARMUP && last_measured == Some(number) { continue; }
                let mut payload = vec![0_u8; config.bytes];
                payload[..8].copy_from_slice(b"TCLDv001");
                payload[8..16].copy_from_slice(&config.run);
                payload[16..20].copy_from_slice(&(index as u32).to_be_bytes());
                payload[20..24].copy_from_slice(&number.to_be_bytes());
                payload[24..32].copy_from_slice(&(config.epoch.elapsed().as_micros() as u64).to_be_bytes());
                sequence = sequence.wrapping_add(1);
                timestamp = timestamp.wrapping_add(960);
                let packet = rtp::Packet { header: rtp::header::Header { version: 2, payload_type: 111, ssrc: 900, sequence_number: sequence, timestamp, ..Default::default() }, payload: payload.into() };
                let result = tokio::time::timeout(Duration::from_millis(100), peer.track.write_rtp(packet)).await;
                if number != WARMUP {
                    let mut stats = stats.lock().unwrap();
                    let scheduled = start_time.unwrap() + PERIOD * last_measured.map_or(0, |n| n + 1);
                    let lag_us = now.saturating_duration_since(scheduled).as_micros() as u64;
                    stats.max_scheduling_lag_us = stats.max_scheduling_lag_us.max(lag_us);
                    stats.scheduling_over_20ms += u64::from(lag_us > 20_000);
                    stats.attempted += 1;
                    if matches!(result, Ok(Ok(_))) { stats.sent.insert(number as usize); } else { stats.send_errors += 1; }
                    last_measured = Some(number);
                }
            }
        }
    }
}

async fn selected_route(pc: &dyn PeerConnection) -> Result<(String, String)> {
    let senders = pc.get_senders().await;
    let transport = senders
        .first()
        .ok_or("Missing sender")?
        .transport()
        .await
        .map_err(|_| "Missing DTLS transport")?
        .ok_or("Missing DTLS transport")?;
    let pair = transport
        .ice_transport()
        .get_selected_candidate_pair()
        .await
        .map_err(|_| "Cannot inspect ICE route")?
        .ok_or("No selected ICE route")?;
    Ok((pair.local().typ.to_string(), pair.remote().typ.to_string()))
}

fn warmed(config: &Config, stats: &[Arc<Mutex<Stats>>]) -> bool {
    stats.iter().enumerate().all(|(index, s)| {
        let s = s.lock().unwrap();
        let first = index / config.room_size * config.room_size;
        (first..(first + config.room_size).min(config.users)).all(|source| {
            source == index
                || (s.streams[source % config.room_size].warm
                    && (!config.video() || s.video_streams[source % config.room_size].warm))
        })
    })
}

fn report(
    config: &Config,
    stats: &[Arc<Mutex<Stats>>],
    failures: usize,
    complete: bool,
) -> (serde_json::Value, bool) {
    let stats: Vec<_> = stats.iter().map(|s| s.lock().unwrap()).collect();
    let mut deliveries = Vec::new();
    let mut video_deliveries = Vec::new();
    let mut histogram = vec![0; 1002];
    let mut video_histogram = vec![0; 1002];
    for (receiver, s) in stats.iter().enumerate() {
        let first = receiver / config.room_size * config.room_size;
        for (sender, source_stats) in stats
            .iter()
            .enumerate()
            .take((first + config.room_size).min(config.users))
            .skip(first)
        {
            if sender == receiver {
                continue;
            }
            let stream = &s.streams[sender % config.room_size];
            for (total, count) in histogram.iter_mut().zip(&stream.histogram) {
                *total += count;
            }
            deliveries.push(Delivery {
                receiver,
                sender,
                expected: source_stats.sent.count(),
                missing: source_stats.sent.missing_from(&stream.received),
                received_unique: stream.received.count(),
                late: stream.late,
                duplicates: stream.duplicates,
                reordered: stream.reordered,
                path_p99_ms: percentile(&stream.histogram, 99),
            });
            if config.video() {
                let stream = &s.video_streams[sender % config.room_size];
                for (total, count) in video_histogram.iter_mut().zip(&stream.histogram) {
                    *total += count;
                }
                video_deliveries.push(Delivery {
                    receiver,
                    sender,
                    expected: source_stats.video_sent.count(),
                    missing: source_stats.video_sent.missing_from(&stream.received),
                    received_unique: stream.received.count(),
                    late: stream.late,
                    duplicates: stream.duplicates,
                    reordered: stream.reordered,
                    path_p99_ms: percentile(&stream.histogram, 99),
                });
            }
        }
    }
    let expected: u64 = deliveries.iter().map(|s| s.expected).sum();
    let missing: u64 = deliveries.iter().map(|s| s.missing).sum();
    let late: u64 = deliveries.iter().map(|s| s.late).sum();
    let sent: u64 = stats.iter().map(|s| s.sent.count()).sum();
    let attempted: u64 = stats.iter().map(|s| s.attempted).sum();
    let scheduled = (config.users * config.seconds * 50) as u64;
    let send_errors: u64 = stats.iter().map(|s| s.send_errors).sum();
    let invalid: u64 = stats.iter().map(|s| s.invalid).sum();
    let skipped = scheduled.saturating_sub(attempted);
    let video_scheduled = if config.video() {
        (config.users * config.video_packets()) as u64
    } else {
        0
    };
    let video_attempted: u64 = stats.iter().map(|s| s.video_attempted).sum();
    let video_skipped = video_scheduled.saturating_sub(video_attempted);
    let video_expected: u64 = video_deliveries.iter().map(|s| s.expected).sum();
    let video_missing: u64 = video_deliveries.iter().map(|s| s.missing).sum();
    let video_late: u64 = video_deliveries.iter().map(|s| s.late).sum();
    // This is a configurable workload's conservative transport check, not a capacity guarantee.
    // Every packet the generator did send arrived on time on every stream. Loss
    // is counted against successful sends, so generator skips are not loss.
    let delivered = |streams: &[Delivery]| {
        streams
            .iter()
            .all(|s| s.expected > 0 && (s.missing + s.late) * 1000 <= s.expected)
    };
    let server_side_passed = complete
        && failures == 0
        && invalid == 0
        && expected > 0
        && delivered(&deliveries)
        && (!config.video() || (video_expected > 0 && delivered(&video_deliveries)));
    // The full verdict also requires the generator to have kept its schedule.
    let passed = server_side_passed
        && send_errors == 0
        && skipped * 1000 <= scheduled
        && (!config.video() || video_skipped * 1000 <= video_scheduled);
    // Built separately: one json! literal this large exceeds the macro recursion limit.
    let video = config.video().then(|| {
        json!({
            "fps": VIDEO_FPS, "packets_per_frame": config.video_packets_per_frame(),
            "packet_bytes": config.video_packet_bytes(), "scheduled": video_scheduled,
            "attempted": video_attempted, "generator_skipped": video_skipped,
            "expected_deliveries": video_expected, "missing": video_missing, "late": video_late,
            "missing_percent": if video_expected == 0 { None } else { Some(video_missing as f64 * 100.0 / video_expected as f64) },
            "path_p50_ms": percentile(&video_histogram, 50), "path_p95_ms": percentile(&video_histogram, 95), "path_p99_ms": percentile(&video_histogram, 99),
            "streams": video_deliveries,
        })
    });
    (
        json!({
            "format_version": 1, "complete": complete, "passed": passed, "server_side_passed": server_side_passed,
            "users": config.users, "room_size": config.room_size, "duration_seconds": config.seconds,
            "payload_bytes": config.bytes, "packets_per_second_per_sender": 50,
            "late_threshold_ms": config.late_ms, "relay_required": config.relay,
            "failed_clients": failures, "scheduled": scheduled, "attempted": attempted, "sent": sent,
            "generator_skipped": skipped, "send_errors": send_errors, "invalid_or_cross_room": invalid,
            "generator_max_scheduling_lag_us": stats.iter().map(|s| s.max_scheduling_lag_us).max(),
            "generator_scheduling_over_20ms": stats.iter().map(|s| s.scheduling_over_20ms).sum::<u64>(),
            "expected_deliveries": expected, "missing": missing, "late": late,
            "missing_percent": if expected == 0 { None } else { Some(missing as f64 * 100.0 / expected as f64) },
            "path_p50_ms": percentile(&histogram, 50), "path_p95_ms": percentile(&histogram, 95), "path_p99_ms": percentile(&histogram, 99),
        "latency_note": "Generator send to generator receive via SFU, single monotonic clock; includes both network legs, queues and client processing. 1001 means >=1001ms. Not server-only delay.",
        "camera_bitrate": config.camera_bitrate,
        "video": video,
        "routes": stats.iter().enumerate().map(|(index, s)| json!({"client": index, "local_candidate": s.local_candidate, "remote_candidate": s.remote_candidate})).collect::<Vec<_>>(),
        "streams": deliveries
        }),
        passed,
    )
}

async fn run() -> Result<bool> {
    let mut args = Args::parse()?;
    if args.flag("--help") {
        println!(
            "voice_load --url http://DESKTOP:3001 --bind MAC_LAN_IP:0 --users 8 --seconds 60 --allow-insecure\nOptions: --room-size 2..8 (8), --users 2..512 (8), --seconds 1..3600 (60),\n--payload-bytes 32..1200 (80), --late-ms 1..1000 (100), --camera-kbps 0..4000 (0, off), --relay, --output report.json.\nRequires THISCORD_LOAD_SECRET matching voice_load_server. Synthetic RTP, no microphones. See docs/voice-load.md."
        );
        return Ok(true);
    }
    let url = args.text("--url", "http://127.0.0.1:3001");
    let bind: SocketAddr = args
        .text("--bind", "127.0.0.1:0")
        .parse()
        .map_err(|_| "--bind must be a local interface IP:0")?;
    if bind.port() != 0 || bind.ip().is_unspecified() {
        return Err("--bind requires a specific interface IP and port 0");
    }
    let users = args.number("--users", 8, 2, 512)?;
    let room_size = args.number("--room-size", 8, 2, 8)?;
    if users % room_size == 1 {
        return Err(
            "Last room would have only one participant; choose a different user count or room size",
        );
    }
    let seconds = args.number("--seconds", 60, 1, 3600)?;
    let bytes = args.number("--payload-bytes", 80, 32, 1200)?;
    let late_ms = args.number("--late-ms", 100, 1, 1000)? as u64;
    let camera_bitrate = args.number("--camera-kbps", 0, 0, 4000)? * 1000;
    let relay = args.flag("--relay");
    let insecure = args.flag("--allow-insecure");
    let output = args.text("--output", "");
    args.finish()?;
    // Reserve the requested artifact before generating traffic; never overwrite prior results.
    let mut output = if output.is_empty() {
        None
    } else {
        Some(
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .map_err(|_| "Cannot create output file (must not already exist)")?,
        )
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = Arc::new(Config {
        endpoint: endpoint(&url, insecure)?,
        bind: bind.to_string(),
        secret: common::secret()?,
        users,
        room_size,
        seconds,
        bytes,
        late_ms,
        relay,
        camera_bitrate,
        epoch: Instant::now(),
        run: uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap(),
    });
    let stats: Vec<_> = (0..users)
        .map(|_| {
            Arc::new(Mutex::new(Stats::new(
                seconds * 50,
                config.video_packets(),
                room_size,
            )))
        })
        .collect();
    let (stop_tx, stop_rx) = watch::channel(false);
    let (start_tx, start_rx) = watch::channel(None);
    let (ready_tx, mut ready_rx) = mpsc::channel(users);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut jobs = tokio::task::JoinSet::new();
    println!(
        "Joining {users} clients in {} rooms ({} bytes every 20ms)...",
        users.div_ceil(room_size),
        bytes
    );
    for (index, stats) in stats.iter().enumerate() {
        let (config, stats, semaphore, ready, start, mut stop) = (
            config.clone(),
            stats.clone(),
            semaphore.clone(),
            ready_tx.clone(),
            start_rx.clone(),
            stop_rx.clone(),
        );
        jobs.spawn(async move {
            let permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| "Coordinator stopped")?;
            if *stop.borrow() { return Err("Cancelled before join"); }
            // Dropping a partially negotiated connection closes its transport/readers.
            let peer = tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(45), join(config.clone(), index, stats.clone())) => result.unwrap_or(Err("Join timeout")),
                _ = stop.changed() => Err("Cancelled during join"),
            };
            drop(permit);
            let _ = ready.send(peer.is_ok()).await;
            let mut peer = match peer {
                Ok(peer) => peer,
                Err(e) => {
                    eprintln!("Client {index}: {e}");
                    return Err(e);
                }
            };
            let result = exercise(&mut peer, &config, index, &stats, start, stop).await;
            peer.close().await;
            if let Err(e) = result {
                eprintln!("Client {index}: {e}");
            }
            result
        });
    }
    drop(ready_tx);
    let coordination = async {
        for count in 1..=users {
            if ready_rx.recv().await != Some(true) {
                return Err("At least one client could not join");
            }
            if count % 8 == 0 || count == users {
                println!("Negotiated {count}/{users}");
            }
        }
        tokio::time::timeout(Duration::from_secs(30), async {
            while !warmed(&config, &stats) { tokio::time::sleep(Duration::from_millis(100)).await; }
        }).await.map_err(|_| "Warmup failed: not every expected media path delivered packets (check UDP/ICE/TURN)")?;
        start_tx
            .send(Some(Instant::now() + Duration::from_secs(1)))
            .map_err(|_| "Clients stopped before measurement")?;
        println!("All media paths verified. Measuring for {seconds}s, then draining for 2s.");
        let mut failures = 0;
        let mut progress = tokio::time::interval(Duration::from_secs(5));
        progress.tick().await;
        loop {
            tokio::select! {
                result = jobs.join_next() => match result {
                    Some(result) => failures += usize::from(!matches!(result, Ok(Ok(())))),
                    None => break,
                },
                _ = progress.tick() => {
                    let sent: u64 = stats.iter().map(|s| s.lock().unwrap().sent.count()).sum();
                    println!("Measured sends accepted: {sent}; client failures: {failures}");
                },
            }
        }
        Ok(failures)
    };
    let result = tokio::select! {
        result = coordination => result,
        _ = tokio::signal::ctrl_c() => Err("Interrupted"),
    };
    let complete = result.as_ref().is_ok_and(|failures| *failures == 0);
    let mut failures = result.as_ref().copied().unwrap_or(0);
    if let Err(e) = result {
        eprintln!("{e}");
    }
    let _ = stop_tx.send(true);
    if tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = jobs.join_next().await {
            failures += usize::from(!matches!(result, Ok(Ok(()))));
        }
    })
    .await
    .is_err()
    {
        failures += jobs.len();
        jobs.shutdown().await;
    }
    let (report, passed) = report(&config, &stats, failures, complete);
    println!(
        "{}: sent={}, expected deliveries={}, missing={}, late={}, generator skipped={}, path p99={}ms",
        if passed { "PASS" } else { "FAIL/INCOMPLETE" },
        report["sent"],
        report["expected_deliveries"],
        report["missing"],
        report["late"],
        report["generator_skipped"],
        report["path_p99_ms"]
    );
    if config.video() {
        let video = &report["video"];
        println!(
            "Video: expected deliveries={}, missing={}, late={}, generator skipped={}, path p50={}ms p99={}ms",
            video["expected_deliveries"],
            video["missing"],
            video["late"],
            video["generator_skipped"],
            video["path_p50_ms"],
            video["path_p99_ms"]
        );
    }
    if let Some(file) = &mut output {
        serde_json::to_writer_pretty(file, &report).map_err(|_| "Cannot write report")?;
    }
    Ok(passed)
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(true) => {}
        Ok(false) => std::process::exit(2),
        Err(e) => {
            eprintln!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_rejects_secret_bearing_urls_and_unapproved_cleartext() {
        assert!(endpoint("http://localhost:3001", false).is_err());
        assert!(endpoint("https://user:secret@example.com", false).is_err());
        assert!(endpoint("https://example.com/?token=secret", false).is_err());
        assert_eq!(
            endpoint("https://example.com", false).unwrap(),
            "wss://example.com/api/v1/voice"
        );
    }

    fn config() -> Config {
        Config {
            endpoint: String::new(),
            bind: String::new(),
            secret: String::new(),
            users: 4,
            room_size: 2,
            seconds: 1,
            bytes: 80,
            late_ms: 100,
            relay: false,
            camera_bitrate: 0,
            epoch: Instant::now(),
            run: [1; 8],
        }
    }

    #[test]
    fn report_cannot_pass_a_missing_stream_or_an_underdriven_generator() {
        let c = config();
        let stats: Vec<_> = (0..4)
            .map(|_| Arc::new(Mutex::new(Stats::new(50, 0, 2))))
            .collect();
        for s in &stats {
            let mut s = s.lock().unwrap();
            for n in 0..50 {
                s.sent.insert(n);
            }
            s.attempted = 50;
        }
        let (r, pass) = report(&c, &stats, 0, true);
        assert!(!pass);
        assert_eq!(r["expected_deliveries"], 200);
        assert_eq!(r["missing"], 200);
        for (index, s) in stats.iter().enumerate() {
            for n in 0..50 {
                s.lock().unwrap().streams[1 - index % 2].record(n, 5, 100);
            }
        }
        assert!(report(&c, &stats, 0, true).1);
        assert!(!report(&c, &stats, 1, true).1);
        assert!(!report(&c, &stats, 0, false).1);
        // An underdriven generator fails the run but not the server-side verdict.
        stats[0].lock().unwrap().attempted = 0;
        let (r, pass) = report(&c, &stats, 0, true);
        assert!(!pass);
        assert_eq!(r["server_side_passed"], true);
    }

    #[test]
    fn video_streams_are_accounted_separately_and_gate_the_verdict() {
        let mut c = config();
        c.camera_bitrate = 1_000_000;
        // 1 Mbit/s at 30 fps in equal packets of at most 1100 bytes.
        assert_eq!(c.video_packets_per_frame(), 4);
        assert_eq!(c.video_packet_bytes(), 1041);
        let video = c.video_packets();
        let stats: Vec<_> = (0..4)
            .map(|_| Arc::new(Mutex::new(Stats::new(50, video, 2))))
            .collect();
        for (index, s) in stats.iter().enumerate() {
            let mut s = s.lock().unwrap();
            for n in 0..50 {
                s.sent.insert(n);
                s.streams[1 - index % 2].record(n, 5, 100);
            }
            s.attempted = 50;
            for n in 0..video {
                s.video_sent.insert(n);
            }
            s.video_attempted = video as u64;
        }
        // Audio alone is complete; every camera stream is still missing.
        let (r, pass) = report(&c, &stats, 0, true);
        assert!(!pass);
        assert_eq!(r["video"]["expected_deliveries"], 4 * video);
        assert_eq!(r["video"]["missing"], 4 * video);
        for (index, s) in stats.iter().enumerate() {
            for n in 0..video {
                s.lock().unwrap().video_streams[1 - index % 2].record(n, 5, 100);
            }
        }
        assert!(report(&c, &stats, 0, true).1);
        // Late camera packets fail the run like missing ones.
        stats[0].lock().unwrap().video_streams[1].late = video as u64;
        assert!(!report(&c, &stats, 0, true).1);
        // A video payload carries its NAL byte before the measurement header.
        let mut stats = Stats::new(50, video, 2);
        let mut payload = vec![0; c.video_packet_bytes()];
        payload[0] = 0x65;
        payload[1..9].copy_from_slice(MAGIC);
        payload[9..17].copy_from_slice(&c.run);
        payload[17..21].copy_from_slice(&1_u32.to_be_bytes());
        payload[21..25].copy_from_slice(&3_u32.to_be_bytes());
        receive(&c, 0, &mut stats, &payload);
        assert_eq!(stats.video_streams[1].received.count(), 1);
        assert_eq!(stats.streams[1].received.count(), 0);
        assert_eq!(stats.invalid, 0);
    }

    #[test]
    fn receive_rejects_cross_room_self_and_old_run_payloads() {
        let c = config();
        let mut stats = Stats::new(50, 0, 2);
        let mut payload = vec![0; 80];
        payload[..8].copy_from_slice(b"TCLDv001");
        payload[8..16].copy_from_slice(&c.run);
        payload[16..20].copy_from_slice(&2_u32.to_be_bytes());
        receive(&c, 0, &mut stats, &payload);
        payload[16..20].copy_from_slice(&0_u32.to_be_bytes());
        receive(&c, 0, &mut stats, &payload);
        payload[16..20].copy_from_slice(&1_u32.to_be_bytes());
        payload[8] = 2;
        receive(&c, 0, &mut stats, &payload);
        assert_eq!(stats.invalid, 3);
        assert_eq!(stats.streams[1].received.count(), 0);
        payload[8] = 1;
        receive(&c, 0, &mut stats, &payload);
        assert_eq!(stats.streams[1].received.count(), 1);
    }
}
