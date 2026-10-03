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
    epoch: Instant,
    run: [u8; 8],
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
    if payload.len() != config.bytes || &payload[..8] != b"TCLDv001" || payload[8..16] != config.run
    {
        stats.invalid += 1;
        return;
    }
    let source = u32::from_be_bytes(payload[16..20].try_into().unwrap()) as usize;
    let sequence = u32::from_be_bytes(payload[20..24].try_into().unwrap());
    let sent_us = u64::from_be_bytes(payload[24..32].try_into().unwrap());
    let now = config.epoch.elapsed().as_micros() as u64;
    if source >= config.users
        || source == index
        || source / config.room_size != index / config.room_size
        || sent_us > now
    {
        stats.invalid += 1;
        return;
    }
    let stream = &mut stats.streams[source % config.room_size];
    if sequence == WARMUP {
        stream.warm = true;
        return;
    }
    if sequence as usize >= config.seconds * 50 {
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
                sdp: serde_json::to_string(&pc.local_description().await.ok_or("Missing answer")?)
                    .map_err(|_| "Cannot serialize answer")?,
            },
        )
        .await?;
        Ok(track)
    })
    .await
    .unwrap_or(Err("ICE negotiation timed out"));
    match result {
        Ok(track) => Ok(Peer {
            socket,
            connection,
            track,
        }),
        Err(e) => Err(e),
    }
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
                }
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(20) { return Err("Heartbeat timeout"); }
                send(&mut peer.socket, ClientEvent::Ping {}).await?;
            },
            incoming = event(&mut peer.socket) => match incoming? {
                ServerEvent::Pong {} => last_pong = Instant::now(),
                ServerEvent::Participants { .. } => {},
                _ => return Err("Server revoked or rejected voice session"),
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
        (first..(first + config.room_size).min(config.users))
            .all(|source| source == index || s.streams[source % config.room_size].warm)
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
    let mut histogram = vec![0; 1002];
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
    // This is a configurable workload's conservative transport check, not a capacity guarantee.
    let passed = complete
        && failures == 0
        && send_errors == 0
        && invalid == 0
        && skipped * 1000 <= scheduled
        && expected > 0
        && deliveries
            .iter()
            .all(|s| s.expected > 0 && (s.missing + s.late) * 1000 <= s.expected);
    (
        json!({
            "format_version": 1, "complete": complete, "passed": passed,
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
            "voice_load --url http://DESKTOP:3001 --bind MAC_LAN_IP:0 --users 8 --seconds 60 --allow-insecure\nOptions: --room-size 2..8 (8), --users 2..500 (8), --seconds 1..3600 (60),\n--payload-bytes 32..1200 (80), --late-ms 1..1000 (100), --relay, --output report.json.\nRequires THISCORD_LOAD_SECRET matching voice_load_server. Synthetic RTP, no microphones. See docs/voice-load.md."
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
    let users = args.number("--users", 8, 2, 500)?;
    let room_size = args.number("--room-size", 8, 2, 8)?;
    if users % room_size == 1 {
        return Err(
            "Last room would have only one participant; choose a different user count or room size",
        );
    }
    let seconds = args.number("--seconds", 60, 1, 3600)?;
    let bytes = args.number("--payload-bytes", 80, 32, 1200)?;
    let late_ms = args.number("--late-ms", 100, 1, 1000)? as u64;
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
        epoch: Instant::now(),
        run: uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap(),
    });
    let stats: Vec<_> = (0..users)
        .map(|_| Arc::new(Mutex::new(Stats::new(seconds * 50, room_size))))
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
            epoch: Instant::now(),
            run: [1; 8],
        }
    }

    #[test]
    fn report_cannot_pass_a_missing_stream_or_an_underdriven_generator() {
        let c = config();
        let stats: Vec<_> = (0..4)
            .map(|_| Arc::new(Mutex::new(Stats::new(50, 2))))
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
        stats[0].lock().unwrap().attempted = 0;
        assert!(!report(&c, &stats, 0, true).1);
    }

    #[test]
    fn receive_rejects_cross_room_self_and_old_run_payloads() {
        let c = config();
        let mut stats = Stats::new(50, 2);
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
