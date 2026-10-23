use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, State};
use thiscord_frontend::audio::{
    AudioEngine, Command,
    connection::Connection,
    reconnect::{self, Connector, Failure},
};
use thiscord_shared::{ChannelId, GuildId, audio::AudioSettings, voice::*};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use webrtc::{
    media_stream::{
        track_local::TrackLocal,
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

struct Job {
    guild_id: GuildId,
    channel_id: Arc<Mutex<ChannelId>>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Job {
    fn owns(&self, guild: GuildId, channel: ChannelId) -> bool {
        self.guild_id == guild
            && self.channel_id.lock().ok().is_some_and(|id| *id == channel)
            && !self.task.is_finished()
    }
}
#[derive(Default)]
pub struct VoiceState {
    job: tokio::sync::Mutex<Option<Job>>,
    status: Arc<Mutex<VoiceStatus>>,
    pub settings: Arc<Mutex<AudioSettings>>,
}
struct Handler {
    gathered: Arc<Notify>,
    engine: AudioEngine,
    media_state: Arc<MediaState>,
    connection: Connection,
    closed: watch::Receiver<bool>,
    screen: crate::native_screen::ScreenState,
    camera: crate::native_screen::ScreenState,
    keyframes: mpsc::Sender<(MediaKind, usize, u32)>,
}

#[derive(Default)]
struct MediaState {
    connected: AtomicBool,
    failed: AtomicBool,
    changed: Notify,
}
impl MediaState {
    fn observe(&self, state: RTCPeerConnectionState) {
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
        if terminal_peer_state(state) {
            self.failed.store(true, Ordering::Release);
        }
        self.changed.notify_one();
    }

    async fn tick(&self, periodic: &mut tokio::time::Interval) {
        // Start audio as soon as DTLS/ICE connects instead of adding up to
        // another half second. Retain periodic maintenance for stable calls.
        tokio::select! {
            _ = self.changed.notified() => {},
            _ = periodic.tick() => {},
        }
    }
}

fn terminal_peer_state(state: RTCPeerConnectionState) -> bool {
    matches!(
        state,
        RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed
    )
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connection.event(
            "voice_peer_state",
            serde_json::json!({"state":format!("{state:?}")}),
        );
        self.media_state.observe(state);
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let ssrc = track.ssrcs().await.first().copied().unwrap_or(0);
        let screen_receiver = MediaKind::from_relay_ssrc(ssrc)
            .filter(|(kind, _)| kind.is_video())
            .map(|(kind, slot)| {
                if kind == MediaKind::Camera {
                    &self.camera
                } else {
                    &self.screen
                }
                .receiver(self.connection.clone(), slot)
            });
        let keyframes = self.keyframes.clone();
        let engine = self.engine.clone();
        let connection = self.connection.clone();
        let mut closed = self.closed.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if !connection.active() {
                    break;
                }
                let event = tokio::select! { biased;
                    _ = closed.changed() => break,
                    _ = tick.tick(), if screen_receiver.is_some() => {
                        let receiver = screen_receiver.as_ref().unwrap();
                        receiver.tick(Instant::now());
                        if let Some(epoch) = receiver.keyframe_request(Instant::now()) && let Some((kind, slot)) = MediaKind::from_relay_ssrc(ssrc) { let _ = keyframes.try_send((kind, slot, epoch)); }
                        continue;
                    },
                    event = track.poll() => match event { Some(event) => event, None => break },
                };
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    let arrived = Instant::now();
                    if let Some(receiver) = &screen_receiver {
                        receiver.push(packet, Instant::now());
                        continue;
                    }
                    let slot = MediaKind::from_relay_ssrc(packet.header.ssrc)
                        .and_then(|(kind, slot)| kind.mixer_slot(slot));
                    if let Some(slot) = slot {
                        if slot < ROOM_CAPACITY && !matches!(packet.header.csrc.len(), 0 | 2) {
                            continue;
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
                            received_at: arrived,
                            payload: packet.payload,
                        });
                    }
                }
            }
        });
    }
}
#[tauri::command]
pub async fn voice_leave(
    app: tauri::AppHandle,
    state: State<'_, VoiceState>,
) -> Result<(), String> {
    let mut guard = state.job.lock().await;
    stop_job(&app, &state, &mut guard).await;
    Ok(())
}
async fn stop_job(app: &tauri::AppHandle, state: &VoiceState, job: &mut Option<Job>) {
    app.state::<crate::native_screen::ScreenState>().clear();
    app.state::<crate::native_camera::CameraState>().clear();
    if let Some(Job { stop, mut task, .. }) = job.take() {
        let _ = stop.send(());
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await; // No old task may overwrite a subsequent join's status.
        }
    }
    app.state::<crate::native_audio::AudioState>()
        .engine
        .notify(Command::Stop);
    crate::native_audio::unregister_ptt(app);
    if let Ok(mut status) = state.status.lock() {
        *status = VoiceStatus {
            message: "Disconnected from voice".into(),
            ..Default::default()
        };
    }
}
#[tauri::command]
pub fn voice_status(state: State<'_, VoiceState>) -> Result<VoiceStatus, String> {
    Ok(state
        .status
        .lock()
        .map_err(|_| "Voice state unavailable")?
        .clone())
}
#[tauri::command]
pub async fn voice_join(
    app: tauri::AppHandle,
    state: State<'_, VoiceState>,
    token: String,
    guild_id: GuildId,
    channel_id: ChannelId,
    settings: AudioSettings,
) -> Result<(), String> {
    settings.validate()?;
    let update_app = app.clone();
    let update_state = update_app.state::<crate::native_update::UpdateState>();
    let _voice_admission = update_state
        .voice_admission
        .try_lock()
        .map_err(|_| "An update or voice connection is already starting")?;
    if crate::native_update::installing(&app) {
        return Err("Wait for the update to finish before joining voice.".into());
    }
    if token.len() != 43 {
        return Err("Sign in before joining voice".into());
    }
    let mut job = state.job.lock().await;
    // This task owns setup, connected media, local recovery and retry backoff.
    // Repeated IPC/UI joins cannot cancel it and create competing supervisors.
    if job
        .as_ref()
        .is_some_and(|job| job.owns(guild_id, channel_id))
    {
        return Ok(());
    }
    stop_job(&app, &state, &mut job).await;
    crate::native_audio::register_ptt(&app, &settings)?;
    *state
        .settings
        .lock()
        .map_err(|_| "Voice settings unavailable")? = settings.clone();
    *state
        .status
        .lock()
        .map_err(|_| "Voice status unavailable")? = VoiceStatus {
        channel_id: Some(channel_id),
        message: "Connecting to voice…".into(),
        ..Default::default()
    };
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    let status = state.status.clone();
    let configuration = state.settings.clone();
    let (stop, stopped) = oneshot::channel();
    let current_channel = Arc::new(Mutex::new(channel_id));
    let task_channel = current_channel.clone();
    let task = tokio::spawn(async move {
        let mut runtime = Runtime {
            app: app.clone(),
            current_channel: task_channel,
            capture_limit: true,
            token,
            guild_id,
            channel_id,
            engine,
            settings: configuration,
            status: status.clone(),
            attempt: Attempt::default(),
        };
        let result = reconnect::supervise(&mut runtime, stopped, rand::random::<u32>).await;
        runtime.close().await;
        crate::native_audio::unregister_ptt(&app);
        if let Ok(mut s) = status.lock() {
            *s = VoiceStatus {
                message: result
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Disconnected from voice".into()),
                ..Default::default()
            };
        }
    });
    *job = Some(Job {
        guild_id,
        channel_id: current_channel,
        stop,
        task,
    });
    Ok(())
}
struct Attempt {
    moved_to: Option<ChannelId>,
    capture_allowed: bool,
    call_id: Option<thiscord_shared::RequestId>,
    signaling: Option<crate::native_signaling::Sender>,
    recovery: Option<crate::native_signaling::recovery::Session>,
    peer: Option<Arc<dyn PeerConnection>>,
    connection: Connection,
    closed: watch::Sender<bool>,
    connected_at: Option<Instant>,
    started_at: Instant,
    last_received: Option<Instant>,
    received_messages: u64,
    stage: &'static str,
    finished: bool,
}
impl Default for Attempt {
    fn default() -> Self {
        Self {
            moved_to: None,
            capture_allowed: false,
            call_id: None,
            signaling: None,
            recovery: None,
            peer: None,
            connection: Connection::default(),
            closed: watch::channel(false).0,
            connected_at: None,
            started_at: Instant::now(),
            last_received: None,
            received_messages: 0,
            stage: "idle",
            finished: false,
        }
    }
}
impl Attempt {
    fn finish(&mut self, reason: &'static str, retryable: bool, failed: bool) {
        if self.finished || self.stage == "idle" {
            return;
        }
        self.finished = true;
        self.connection.event(
            "voice_attempt_finished",
            serde_json::json!({
                "reason":reason,"retryable":retryable,"failed":failed,"stage":self.stage,
                "elapsed_ms":self.started_at.elapsed().as_millis(),
                "connected_ms":self.connected_at.map(|at|at.elapsed().as_millis()),
                "signaling_age_ms":self.last_received.map(|at|at.elapsed().as_millis()),
                "received_messages":self.received_messages,
            }),
        );
    }
    async fn close_signaling(&mut self) {
        if let Some(mut recovery) = self.recovery.take() {
            recovery.close().await;
        }
        if let Some(mut send) = self.signaling.take() {
            // A raw TCP drop through a proxy can leave the origin's membership
            // alive until its heartbeat expires. Replacement admission fences it.
            // Retain the sink across run()/supervise() cancellation so all exits
            // explicitly release that membership before retrying.
            let outcome = tokio::time::timeout(Duration::from_millis(500), async {
                send.send(frame(ClientEvent::Leave {}).expect("leave serializes"))
                    .await?;
                send.close().await
            })
            .await;
            let error = match &outcome {
                Ok(Ok(())) => serde_json::Value::Null,
                Ok(Err(error)) => crate::native_signaling::diagnostics::socket(error),
                Err(_) => serde_json::json!({"kind":"timeout"}),
            };
            self.connection.event(
                "voice_signaling_closed",
                serde_json::json!({"sent":matches!(outcome, Ok(Ok(()))),"error":error}),
            );
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.finish("cancelled", false, false);
        self.connection.close();
        self.closed.send_replace(true);
    }
}
struct Runtime {
    current_channel: Arc<Mutex<ChannelId>>,
    capture_limit: bool,
    app: tauri::AppHandle,
    token: String,
    guild_id: GuildId,
    channel_id: ChannelId,
    engine: AudioEngine,
    settings: Arc<Mutex<AudioSettings>>,
    status: Arc<Mutex<VoiceStatus>>,
    attempt: Attempt,
}
impl Runtime {
    async fn close(&mut self) {
        self.attempt.finish("cancelled", false, false);
        self.attempt
            .connection
            .event("voice_transport_closing", serde_json::json!({}));
        self.app
            .state::<crate::native_screen::ScreenState>()
            .clear();
        self.app
            .state::<crate::native_camera::CameraState>()
            .clear();
        self.attempt.connection.close();
        self.attempt.closed.send_replace(true);
        self.engine.notify(Command::Stop);
        self.engine.notify(Command::Pressed(false));
        self.attempt.close_signaling().await;
        if let Some(peer) = &self.attempt.peer {
            let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
        }
        self.attempt.peer = None;
    }
}
impl Connector for Runtime {
    async fn attempt(&mut self) -> (Failure, bool) {
        let call_id = self.attempt.call_id;
        self.attempt = Attempt::default();
        self.attempt.call_id = call_id;
        self.attempt.capture_allowed = self.capture_limit;
        self.attempt.stage = "setup";
        self.attempt
            .connection
            .event("voice_attempt_started", serde_json::json!({}));
        let configuration = self.settings.lock().map(|s| s.clone());
        let registration = configuration
            .map_err(|_| "Settings unavailable".to_string())
            .and_then(|s| crate::native_audio::register_ptt(&self.app, &s));
        if let Err(error) = registration {
            self.attempt.finish("setup_failed", false, true);
            return (error.into(), false);
        }
        if let Ok(mut status) = self.status.lock() {
            *status = VoiceStatus {
                channel_id: Some(self.channel_id),
                message: "Connecting to voice...".into(),
                ..Default::default()
            };
        }
        let result = run(
            &self.app,
            &self.token,
            self.guild_id,
            self.channel_id,
            self.settings.clone(),
            self.status.clone(),
            &mut self.attempt,
        )
        .await;
        self.attempt.finish(
            result
                .as_ref()
                .err()
                .map(Failure::diagnostic_reason)
                .unwrap_or("completed"),
            result.as_ref().err().is_some_and(|e| e.retryable),
            result.is_err(),
        );
        let stable = self
            .attempt
            .connected_at
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(30));
        if let Ok(mut status) = self.status.lock() {
            status.connected = false;
            status.participants.clear();
        }
        let moved_to = self.attempt.moved_to;
        self.close().await;
        if let Some(destination) = moved_to {
            self.capture_limit &= self.attempt.capture_allowed;
            self.channel_id = destination;
            if let Ok(mut current) = self.current_channel.lock() {
                *current = destination;
            }
            self.attempt.call_id = None;
            return (
                Failure::temporary(
                    "Moved by a moderator. Reconnecting with your existing mute/deafen settings.",
                )
                .with_reason("moderator_move"),
                true,
            );
        }
        (
            result
                .err()
                .unwrap_or_else(|| Failure::from("Disconnected from voice")),
            stable,
        )
    }
    fn waiting(&mut self, error: &Failure, delay: Duration) {
        self.attempt.connection.event(
            "voice_retry_wait",
            serde_json::json!({"reason":error.diagnostic_reason(),"delay_ms":delay.as_millis()}),
        );
        if let Ok(mut status) = self.status.lock() {
            *status = VoiceStatus {
                channel_id: Some(self.channel_id),
                message: format!(
                    "Reconnecting in {:.1}s - {}",
                    delay.as_secs_f32(),
                    error.message
                ),
                ..Default::default()
            };
        }
    }
}
pub(super) async fn send_event<
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
>(
    send: &mut S,
    event: ClientEvent,
    connection: &Connection,
) -> Result<(), Failure> {
    tokio::time::timeout(Duration::from_secs(3), send.send(frame(event)?))
        .await
        .map_err(|_| {
            connection.event(
                "voice_socket_error",
                serde_json::json!({"operation":"send", "error":{"kind":"timeout"}}),
            );
            Failure::temporary("Voice signaling sender stalled")
                .with_reason("signaling_send_timeout")
        })?
        .map_err(|error| socket_failure(connection, "send", &error))
}
pub(super) fn socket_failure(
    connection: &Connection,
    operation: &'static str,
    error: &tokio_tungstenite::tungstenite::Error,
) -> Failure {
    connection.event("voice_socket_error", serde_json::json!({"operation":operation,"error":crate::native_signaling::diagnostics::socket(error)}));
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};
    let recoverable = matches!(
        error,
        Error::Io(_)
            | Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
    );
    let failure = if recoverable {
        Failure::temporary("Voice socket failed")
    } else {
        Failure::from("Voice signaling protocol failed")
    };
    failure.with_reason(if operation == "send" {
        "signaling_send_failed"
    } else {
        "signaling_receive_failed"
    })
}
pub(super) fn log_close(connection: &Connection, message: &Message) {
    if let Message::Close(frame) = message {
        connection.event(
            "voice_signaling_close_received",
            serde_json::json!({"close_code":frame.as_ref().map(|f|u16::from(f.code))}),
        );
    }
}
pub(super) fn server_failure(connection: &Connection, error: thiscord_shared::ApiError) -> Failure {
    connection.event(
        "voice_server_error",
        serde_json::json!({"code":error.code,"error_request_id":error.request_id}),
    );
    Failure::server(error)
}
fn frame(event: ClientEvent) -> Result<Message, String> {
    Ok(Message::Text(
        serde_json::to_string(&ClientFrame {
            version: VOICE_VERSION,
            event,
        })
        .map_err(|_| "Invalid signaling message")?
        .into(),
    ))
}
pub(super) fn closed(message: &Message) -> Failure {
    if let Message::Close(Some(frame)) = message {
        let code = u16::from(frame.code);
        if code == REPLACED_CLOSE_CODE {
            return Failure::replaced();
        }
        if !matches!(code, 1000 | 1001 | 1006 | 1011..=1014) {
            // Policy/protocol closes are final; never display arbitrary close bodies.
            return Failure::from("Voice connection rejected or protocol unsupported")
                .with_reason("signaling_policy_close");
        }
    }
    Failure::temporary("Voice connection closed").with_reason("signaling_close")
}
fn bind() -> Result<String, String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.connect("192.0.2.1:9").map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:0",
        s.local_addr().map_err(|e| e.to_string())?.ip()
    ))
}
async fn run(
    app: &tauri::AppHandle,
    token: &str,
    guild_id: GuildId,
    channel_id: ChannelId,
    settings: Arc<Mutex<AudioSettings>>,
    status: Arc<Mutex<VoiceStatus>>,
    attempt: &mut Attempt,
) -> Result<(), Failure> {
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
    attempt.stage = "connect";
    let ws = crate::native_signaling::connect_for(base, &attempt.connection).await?;
    attempt.stage = "offer";
    let reconnect = attempt.call_id.is_some();
    let request_id = attempt
        .connection
        .request_id()
        .ok_or("Voice server did not provide a request ID")?;
    let call_id = *attempt.call_id.get_or_insert(request_id);
    let (send, mut receive) = ws.split();
    attempt.signaling = Some(send);
    let send = attempt.signaling.as_mut().unwrap();
    send_event(
        send,
        ClientEvent::Join {
            token: token.into(),
            guild_id,
            channel_id,
            attempt: Some(JoinAttempt { call_id, reconnect }),
        },
        &attempt.connection,
    )
    .await?;
    let first = tokio::time::timeout(
        Duration::from_secs(20),
        crate::native_signaling::next_data(&mut receive),
    )
    .await
    .map_err(|_| Failure::temporary("Voice offer timed out").with_reason("offer_timeout"))?
    .ok_or_else(|| Failure::temporary("Voice socket closed").with_reason("signaling_eof"))?
    .map_err(|error| socket_failure(&attempt.connection, "receive_offer", &error))?;
    attempt.last_received = Some(Instant::now());
    attempt.received_messages += 1;
    if first.is_close() {
        log_close(&attempt.connection, &first);
        return Err(closed(&first));
    }
    let first: ServerFrame = serde_json::from_str(first.to_text().map_err(|_| "Invalid offer")?)
        .map_err(|_| "Invalid offer")?;
    if first.version != VOICE_VERSION {
        return Err("Unsupported voice version".into());
    }
    let (
        sdp,
        own_slot,
        can_speak,
        ice_servers,
        screen_video,
        camera_video,
        screen_feedback,
        screen_subscriptions,
        screen_layers,
        resume_token,
        remote_audio,
        packet_redundancy,
    ) = match first.event {
        ServerEvent::Offer {
            packet_redundancy,
            sdp,
            slot,
            can_speak,
            ice_servers,
            screen_video,
            camera_video,
            screen_feedback,
            screen_subscriptions,
            screen_layers,
            resume_token,
            remote_audio,
        } if first.version == VOICE_VERSION && slot < ROOM_CAPACITY => (
            sdp,
            slot,
            can_speak,
            ice_servers,
            screen_video,
            camera_video,
            screen_feedback,
            screen_subscriptions,
            screen_layers,
            resume_token,
            remote_audio,
            packet_redundancy,
        ),
        ServerEvent::Replaced {} => return Err(Failure::replaced()),
        ServerEvent::Error { error } => return Err(server_failure(&attempt.connection, error)),
        _ => return Err("Invalid voice offer".into()),
    };
    let can_speak = can_speak && attempt.capture_allowed;
    attempt.capture_allowed = can_speak;
    attempt.connection.event(
        "voice_offer_received",
        serde_json::json!({
            "elapsed_ms":attempt.started_at.elapsed().as_millis()
        }),
    );
    attempt.stage = "negotiate";
    let negotiation_started = Instant::now();
    let gathered = Arc::new(Notify::new());
    let media_state = Arc::new(MediaState::default());
    let mut media = MediaEngine::default();
    media
        .register_codec(
            rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
                rtp_codec: rtc::rtp_transceiver::rtp_sender::RTCRtpCodec {
                    mime_type: "video/H264".into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                    ..Default::default()
                },
                payload_type: MediaKind::ScreenVideo.payload_type(),
            },
            rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video,
        )
        .expect("valid screen codec");
    if packet_redundancy {
        thiscord_frontend::audio::red::register(&mut media)?;
    }
    media.register_default_codecs().map_err(|e| e.to_string())?;
    let (keyframe_tx, mut keyframe_rx) = mpsc::channel(ROOM_CAPACITY);
    let force_keyframe = Arc::new(AtomicBool::new(false));
    let registry =
        register_default_interceptors(Registry::new(), &mut media).map_err(|e| e.to_string())?;
    let registry =
        thiscord_frontend::audio::adaptive_fec::register(registry, attempt.connection.clone());
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
        .build();
    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_runtime(Arc::new(
                thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
            ))
            .with_configuration(configuration)
            .with_interceptor_registry(registry)
            .with_media_engine(media)
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                engine: engine.clone(),
                media_state: media_state.clone(),
                connection: attempt.connection.clone(),
                closed: attempt.closed.subscribe(),
                keyframes: keyframe_tx,
                camera: app
                    .state::<crate::native_camera::CameraState>()
                    .views
                    .clone(),
                screen: app
                    .state::<crate::native_screen::ScreenState>()
                    .inner()
                    .clone(),
            }))
            .with_udp_addrs(vec![bind().map_err(Failure::temporary)?])
            .build()
            .await
            .map_err(|_| Failure::temporary("Cannot initialize voice media connection"))?,
    );
    attempt.peer = Some(pc.clone());
    let diagnostics_peer = pc.clone();
    let diagnostics_connection = attempt.connection.clone();
    let diagnostics_screen = app
        .state::<crate::native_screen::ScreenState>()
        .inner()
        .clone();
    tokio::spawn(async move {
        while diagnostics_connection.active() {
            if let Ok(report) = tokio::time::timeout(
                Duration::from_millis(100),
                diagnostics_peer.get_stats(Instant::now(), rtc::statistics::StatsSelector::None),
            )
            .await
            {
                diagnostics_connection
                    .metrics
                    .set_network(thiscord_frontend::audio::quality::network(&report));
                diagnostics_screen.network_diagnostics(
                    &diagnostics_connection,
                    thiscord_frontend::screen::metrics::network(&report),
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let offer: RTCSessionDescription = serde_json::from_str(&sdp).map_err(|_| "Invalid SDP")?;
    let packet_redundancy = packet_redundancy && thiscord_shared::rtp_red::negotiated(&offer.sdp);
    pc.set_remote_description(offer)
        .await
        .map_err(|_| "Cannot accept server offer")?;
    let track = thiscord_frontend::audio::transport::track(MediaKind::Microphone.publisher_ssrc());
    pc.add_track(track.clone() as Arc<dyn TrackLocal>)
        .await
        .map_err(|_| "Cannot add microphone track")?;
    let camera_track = thiscord_frontend::screen::track(MediaKind::Camera.publisher_ssrc());
    if camera_video {
        pc.add_track(camera_track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| "Cannot add camera track")?;
    }
    let low_track = thiscord_frontend::screen::track(thiscord_shared::screen::LOW_LAYER_SSRC);
    let screen_track = thiscord_frontend::screen::track(MediaKind::ScreenVideo.publisher_ssrc());
    let system_track =
        thiscord_frontend::audio::transport::track(MediaKind::SystemAudio.publisher_ssrc());
    if screen_layers {
        pc.add_track(low_track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| "Cannot add screen quality track")?;
    }
    if screen_video {
        pc.add_track(screen_track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| "Cannot add screen video track")?;
        pc.add_track(system_track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| "Cannot add shared audio track")?;
    }
    let answer = pc
        .create_answer(None)
        .await
        .map_err(|_| "Cannot create answer")?;
    pc.set_local_description(answer)
        .await
        .map_err(|_| "Cannot set answer")?;
    attempt.connection.event(
        "voice_answer_created",
        serde_json::json!({
            "elapsed_ms":attempt.started_at.elapsed().as_millis(),
            "negotiation_ms":negotiation_started.elapsed().as_millis()
        }),
    );
    let gathering_started = Instant::now();
    tokio::time::timeout(Duration::from_secs(8), gathered.notified())
        .await
        .map_err(|_| Failure::temporary("ICE gathering timed out"))?;
    let gathering_wait = gathering_started.elapsed();
    let sdp = serde_json::to_string(&pc.local_description().await.ok_or("Missing answer")?)
        .map_err(|_| "Invalid answer")?;
    send_event(
        send,
        ClientEvent::Answer {
            packet_redundancy,
            remote_audio,
            sdp,
            screen_feedback,
            screen_subscriptions,
            screen_layers,
        },
        &attempt.connection,
    )
    .await?;
    attempt.connection.event(
        "voice_answer_sent",
        serde_json::json!({
            "elapsed_ms":attempt.started_at.elapsed().as_millis(),
            "negotiation_ms":negotiation_started.elapsed().as_millis(),
            "gathering_wait_ms":gathering_wait.as_millis()
        }),
    );
    attempt.recovery = Some(crate::native_signaling::recovery::Session::start(
        attempt.signaling.take().unwrap(),
        receive,
        crate::native_signaling::recovery::Credentials {
            base: base.into(),
            token: token.into(),
            guild_id,
            channel_id,
            resume_token,
        },
        attempt.connection.clone(),
    ));
    let signaling = attempt.recovery.as_mut().unwrap();
    attempt.stage = "media_connect";
    let (tx, mut outgoing) = mpsc::channel::<thiscord_frontend::audio::outgoing::EncodedFrame>(
        thiscord_frontend::audio::outgoing::QUEUE_CAPACITY,
    );
    let transmit_control = engine.outgoing_control();
    if packet_redundancy {
        transmit_control.enable_receive_protection();
        transmit_control.pause_receive();
    } else {
        transmit_control.disable_receive_protection();
    }
    let mut audio_started = false;
    let mut audio_control = thiscord_frontend::audio::voice_control::VoiceControl::default();
    let mut roster: Vec<Participant> = Vec::new();
    let mut roster_compatibility = thiscord_frontend::audio::red::RosterCompatibility::default();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let start = Instant::now();
    let mut screen_state = Some((false, false));
    let mut camera_state = Some(false);
    let mut rtp_clock = thiscord_frontend::audio::outgoing::RtpClock::default();
    let mut boundary = thiscord_frontend::audio::red::boundary::Boundary::default();
    let mut red_sender = thiscord_frontend::audio::red::Sender::default();
    let mut accepted_packets = 0_u64;
    let mut accepted_bytes = 0_u64;
    let mut last_audio_stats = Instant::now();
    let mut remote_jobs = tokio::task::JoinSet::new();
    let mut remote_reply: Option<(
        thiscord_shared::RequestId,
        std::collections::VecDeque<String>,
    )> = None;
    let mut remote_tick = tokio::time::interval(Duration::from_millis(100));
    remote_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if packet_redundancy && audio_started {
            let observed_control = transmit_control.generation();
            if boundary.control(observed_control)? {
                red_sender.clear();
                engine.notify_control(Command::ReceiveBoundary {
                    connection: attempt.connection.clone(),
                    generation: transmit_control.generation(),
                    epoch: None,
                })?;
            }
            if !signaling.recovering() && boundary.needs_refresh() {
                let Some(generation) = transmit_control.refresh_generation(observed_control) else {
                    continue;
                };
                boundary.start_refresh(generation)?;
                red_sender.clear();
                engine.notify_control(Command::ReceiveBoundary {
                    connection: attempt.connection.clone(),
                    generation,
                    epoch: None,
                })?;
            }
            if signaling.recovering() {
                boundary.resume();
                transmit_control.pause_receive();
            } else if let Some((generation, challenge)) = match boundary.request(Instant::now()) {
                Ok(request) => request,
                Err(
                    thiscord_frontend::audio::red::boundary::RequestError::AcknowledgmentTimeout,
                ) => {
                    signaling.recover();
                    boundary.resume();
                    transmit_control.pause_receive();
                    red_sender.clear();
                    continue;
                }
                Err(error) => return Err(<&'static str>::from(error).into()),
            } {
                signaling.send(match challenge {
                    Some(challenge) => ClientEvent::MicrophoneRefresh {
                        generation,
                        challenge,
                    },
                    None => ClientEvent::MicrophoneBoundary { generation },
                });
            }
        }
        tokio::select! {
            _ = boundary.wait_deadline(), if packet_redundancy && audio_started && !signaling.recovering() => {},
            result = audio_control.completed() => {
                let audio = result?;
                if !audio_started {
                        audio_started=true;attempt.connected_at=Some(Instant::now());attempt.stage="active";
                        attempt.connection.event("voice_media_connected",serde_json::json!({}));
                        if screen_video { app.state::<crate::native_screen::ScreenState>().bind(crate::native_screen::Binding {
                            own_slot, local_epoch: Default::default(), local_visible: Default::default(),
                            connection: attempt.connection.clone(), video: screen_track.clone(), audio: system_track.clone(), can_publish: can_speak,
                            subscriptions: screen_subscriptions,
                            layers: screen_layers.then(|| crate::native_screen::LayerBinding::new(low_track.clone())),
                            demand: Default::default(), network_demand: Default::default(),
                            force_keyframe: force_keyframe.clone(), metrics: Default::default(),
                            video_sequence: Default::default(), audio_sequence: Default::default(), audio_timestamp: Default::default(), started: Instant::now(),
                        }); }
                        if camera_video { app.state::<crate::native_camera::CameraState>().bind(crate::native_screen::Binding {
                            own_slot, local_epoch: Default::default(), local_visible: Default::default(),
                            connection: attempt.connection.clone(), video: camera_track.clone(), audio: system_track.clone(), can_publish: can_speak, subscriptions: screen_subscriptions, layers: None,
                            demand: Default::default(), network_demand: Default::default(), force_keyframe: Default::default(), metrics: Default::default(), video_sequence: Default::default(), audio_sequence: Default::default(), audio_timestamp: Default::default(), started: Instant::now(),
                        }); }
                        engine.notify_control(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()})?;
                        if let Ok(mut s)=status.lock(){s.connected=true;s.message=if can_speak{"Connected"}else{"Connected as listener. Leave and join again to enable newly granted microphone access."}.into();}
                } else {
                    if !audio.running {return Err(Failure::from(audio.message).with_reason("audio_stopped"));}
                    if let Ok(mut s)=status.lock(){s.message=audio.message;}
                    // Disconnected is recoverable by ICE itself. Keep this peer
                    // until Failed/Closed; signaling has its own bounded recovery.
                    if signaling.recovering() && let Ok(mut s)=status.lock() {
                        s.message="Recovering voice signaling...".into();
                    } else if !media_state.connected.load(Ordering::Acquire) && let Ok(mut s)=status.lock() {
                        s.message="Recovering voice connection...".into();
                    }
                }
            },
            _ = transmit_control.changed(), if packet_redundancy => { red_sender.clear(); },
            Some(result) = remote_jobs.join_next() => {
                if let Ok((id, chunks)) = result {
                    remote_reply = Some((id, chunks));
                }
            },
            _ = remote_tick.tick(), if remote_reply.is_some() => {
                if let Some((id, chunks)) = remote_reply.as_mut() {
                    if let Some(chunk) = chunks.pop_front() {
                        signaling.send(ClientEvent::AudioReply { request_id: *id, chunk, done: chunks.is_empty() });
                    }
                    if chunks.is_empty() { remote_reply = None; }
                }
            },
            Some((kind, slot, epoch))=keyframe_rx.recv()=>{
                if screen_feedback && crate::native_screen::video_state(app, kind).watching(slot, epoch) && roster.iter().any(|m| m.slot == slot && m.video_epoch(kind) == epoch && m.video_active(kind)) {
                    signaling.send(ClientEvent::ScreenKeyframe { kind, slot, epoch });
                }
            },
            message=signaling.events.recv()=>{
                let message=message.ok_or_else(||Failure::from("Voice signaling worker stopped").with_reason("signaling_worker_stopped"))??;
                attempt.received_messages += 1;
                attempt.last_received = Some(Instant::now());
                if message.is_close(){log_close(&attempt.connection, &message);return Err(closed(&message));}
                if !message.is_text(){continue;}
                let message:ServerFrame=serde_json::from_str(message.to_text().map_err(|_|"Invalid voice event")?).map_err(|_|"Invalid voice event")?;
                if message.version!=VOICE_VERSION{return Err("Unsupported voice version".into());}
                match message.event{
                    ServerEvent::MicrophoneRefreshRequired { challenge } => {
                        if packet_redundancy { boundary.require_refresh(challenge); }
                    },
                    ServerEvent::MicrophoneBoundaryAccepted { generation, microphone_epoch, source_epoch, challenge, accepted } => {
                        let acknowledged_control = transmit_control.generation();
                        if boundary.acknowledge(generation, challenge, accepted) && boundary.ready_control(acknowledged_control) && microphone_epoch != 0 && source_epoch != 0 {
                            engine.notify_control(Command::ReceiveBoundary { connection: attempt.connection.clone(), generation: acknowledged_control, epoch: Some(microphone_epoch) })?;
                            if let Some((generation, challenge)) = boundary.ready_notice() {
                                signaling.send(ClientEvent::MicrophoneRefreshReady { generation, challenge });
                            }
                        }
                    },
                    ServerEvent::Resumed {} => {
                        boundary.resume();
                        red_sender.clear();
                        if packet_redundancy { engine.notify_control(Command::ReceiveBoundary { connection: attempt.connection.clone(), generation: transmit_control.generation(), epoch: None })?; }
                        screen_state = None;
                        camera_state = None;
                        // An interrupted chunked admin reply cannot be replayed
                        // safely. The server rejects that request on reattachment.
                        remote_jobs.abort_all();
                        remote_jobs.detach_all();
                        remote_reply = None;
                    },
                    ServerEvent::AudioCommand { request_id, command } => {
                        if remote_jobs.is_empty() && remote_reply.is_none() {
                            let app = app.clone();
                            let connection = attempt.connection.clone();
                            remote_jobs.spawn(async move {
                                let result = crate::native_remote_audio::execute(app, connection, command).await;
                                let chunks = tokio::task::spawn_blocking(move || crate::native_remote_audio::encode(result)).await
                                    .unwrap_or_else(|_| crate::native_remote_audio::encode(Err("Audio response task failed".into())));
                                (request_id, chunks)
                            });
                        }
                    },
                    ServerEvent::ScreenTarget { kind, epoch, bitrate, layers } => {
                        if status.lock().ok().is_some_and(|s| s.participants.iter().any(|m| m.slot == own_slot && m.video_epoch(kind) == epoch && m.video_active(kind))) {
                            if kind == MediaKind::Camera { app.state::<crate::native_camera::CameraState>().target(bitrate); } else { app.state::<crate::native_screen::ScreenState>().layer_target(bitrate, layers); }
                        }
                    },
                    ServerEvent::MediaDiagnostics{counters}=>{ app.state::<crate::native_screen::ScreenState>().server_diagnostics(counters); },
                    ServerEvent::ScreenKeyframe{kind, epoch, layer}=>{
                        if kind == MediaKind::Camera { app.state::<crate::native_camera::CameraState>().keyframe(); continue; }
                        if status.lock().ok().is_some_and(|s| s.participants.iter().any(|m| m.slot == own_slot && m.screen_epoch == epoch && m.sharing_screen)) { if layer != Some(1) { force_keyframe.store(true, Ordering::Release); }
                            if layer != Some(0) { app.state::<crate::native_screen::ScreenState>().layer_keyframe(); } }
                    },
                    ServerEvent::Moved {channel_id:destination}=>{
                        if destination==channel_id {return Err("Invalid move destination".into());}
                        attempt.moved_to=Some(destination);
                        return Err(Failure::from("Moved by a moderator").with_reason("moderator_move"));
                    },
                    ServerEvent::Participants{members}=>{
                        if members.len()>ROOM_CAPACITY||members.iter().any(|m|m.slot>=ROOM_CAPACITY){return Err("Invalid participant list".into());}
                        roster_compatibility.observe(&members)?;
                        app.state::<crate::native_screen::ScreenState>().roster(if settings.lock().map_err(|_|"Settings unavailable")?.deafened { &[] } else { &members });
                        app.state::<crate::native_camera::CameraState>().views.roster(if settings.lock().map_err(|_|"Settings unavailable")?.deafened { &[] } else { &members });
                        roster = members.iter().filter(|m|m.slot!=own_slot).cloned().collect();
                        engine.notify_control(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()})?;
                        if let Ok(mut s)=status.lock(){s.participants=members;s.own_slot=Some(own_slot);}
                    },ServerEvent::Replaced{}=>return Err(Failure::replaced()),ServerEvent::Error{error}=>return Err(server_failure(&attempt.connection,error)),ServerEvent::Revoked{}=>return Err("Voice access changed. Join again if you still have permission.".into()),_=>{},
                }
            },
            Some(mut frame)=outgoing.recv()=>{
                if !media_state.connected.load(Ordering::Acquire) {
                    red_sender.clear();
                    frame.finish(thiscord_frontend::audio::quality::Count::OutgoingFailedPackets);
                    continue;
                }
                if let Err(reason) = transmit_control.check(&frame, &attempt.connection, Instant::now()) {
                    red_sender.clear();
                    transmit_control.dropped_frame(reason, &mut frame);
                    continue;
                }
                if packet_redundancy {
                    if boundary.frame(&frame)? {
                        // Delivery/device gaps reset the codec even without a UI
                        // setting change. Make their receive fence urgent too.
                        transmit_control.invalidate();
                        red_sender.clear();
                        engine.notify_control(Command::ReceiveBoundary { connection: attempt.connection.clone(), generation: transmit_control.generation(), epoch: None })?;
                    }
                    if !boundary.ready() {
                        red_sender.clear();
                        transmit_control.dropped_frame(thiscord_frontend::audio::outgoing::DropReason::ControlInvalidated, &mut frame);
                        continue;
                    }
                }
                if !red_sender.within_budget(&frame) {
                    frame.finish(thiscord_frontend::audio::quality::Count::OutgoingFailedPackets);
                    continue;
                }
                let (sequence, timestamp) = rtp_clock.pending(frame.media_position);
                let red_enabled = packet_redundancy && frame.packet_redundancy;
                let (payload_type, payload) = red_sender.packet(&frame, red_enabled, Instant::now());
                let payload_bytes = payload.len();
                let packet=rtc::rtp::Packet{header:rtc::rtp::header::Header{version:2,payload_type,ssrc:MediaKind::Microphone.publisher_ssrc(),sequence_number:sequence,timestamp,marker:frame.marker,csrc:if packet_redundancy { boundary.csrc() } else { vec![] },..Default::default()},payload};
                match transmit_control.guard_write_until(&frame, &attempt.connection, red_sender.deadline(&frame), track.write_rtp(packet)).await {
                    Ok(result) => {
                        use thiscord_frontend::audio::quality::Count;
                        frame.finish(if result.is_ok() { Count::OutgoingSentPackets } else { Count::OutgoingFailedPackets });
                        result.map_err(|_|Failure::temporary("Voice media connection failed").with_reason("media_send_failed"))?;
                        // One admitted outer RTP packet. Source 0 belongs to this
                        // unique Connection; privacy tags and RED copies do not
                        // replace its stable FEC feedback identity.
                        attempt.connection.uplink_sent(0, sequence);
                        rtp_clock.next(frame.media_position);
                        accepted_packets += 1;
                        accepted_bytes += payload_bytes as u64;
                        red_sender.admitted(frame, red_enabled);
                    },
                    Err(reason) => { red_sender.clear(); transmit_control.dropped_frame(reason, &mut frame); },
                }
            },
            _=media_state.tick(&mut ticker)=>{
                if last_audio_stats.elapsed() >= Duration::from_secs(5) {
                    attempt.connection.event("voice_transmit", serde_json::json!({
                        "accepted_packets":accepted_packets,"accepted_payload_bytes":accepted_bytes,
                        "minimum_rtp_header_bytes":accepted_packets*12
                    }));
                    last_audio_stats = Instant::now();
                }
                if media_state.failed.load(Ordering::Acquire){
                    attempt.connection.event("voice_connection_lost",serde_json::json!({"peer_failed":true}));
                    return Err(Failure::temporary("Voice connection lost").with_reason("peer_failed"));
                }
                if !audio_started && !audio_control.pending(){
                    if start.elapsed()>Duration::from_secs(15){return Err(Failure::temporary("Media connection timed out").with_reason("media_connect_timeout"));}
                    if media_state.connected.load(Ordering::Acquire){
                        attempt.stage="audio_start";
                        let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();let audio_app=app.clone();let tx=tx.clone();let connection=attempt.connection.clone();
                        audio_control.start(move||crate::native_audio::start_voice(&audio_app,guild_id,connection,configuration,tx,can_speak));
                    }
                }
                let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();
                if audio_started && !audio_control.pending() {
                    let e=engine.clone();
                    let heartbeat_connection=attempt.connection.clone();
                    audio_control.start(move||e.command(Command::Keepalive(heartbeat_connection)));
                }
                if configuration.deafened { crate::native_screen::screen_stop(app.state::<crate::native_screen::ScreenState>()); }
                let camera = app.state::<crate::native_camera::CameraState>();
                if configuration.deafened { camera.stop(None); }
                if camera_video {
                    let active = camera.active();
                    if camera_state != Some(active) { signaling.send(ClientEvent::Camera { active }); camera_state = Some(active); }
                }
                let current_screen = app.state::<crate::native_screen::ScreenState>().sharing();
                if Some(current_screen) != screen_state {
                    signaling.send(ClientEvent::Screen { active: current_screen.0, audio: current_screen.1, quality: screen_layers.then(|| app.state::<crate::native_screen::ScreenState>().quality()) });
                    screen_state = Some(current_screen);
                }
                if packet_redundancy && boundary.control(transmit_control.generation())? {
                    red_sender.clear();
                    engine.notify_control(Command::ReceiveBoundary { connection: attempt.connection.clone(), generation: transmit_control.generation(), epoch: None })?;
                }
                if !packet_redundancy || configuration.muted || configuration.deafened || boundary.ready() {
                    signaling.send(ClientEvent::State{muted:configuration.muted,deafened:configuration.deafened});
                }
                if screen_subscriptions {
                    let mut views = if configuration.deafened { vec![] } else { app.state::<crate::native_screen::ScreenState>().subscriptions() };
                    if !configuration.deafened { views.extend(camera.views.subscriptions()); }
                    signaling.send(ClientEvent::ScreenViews { views });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn peer_changes_wake_audio_without_waiting_for_the_status_interval() {
        let media = MediaState::default();
        let mut periodic = tokio::time::interval(Duration::from_secs(60));
        periodic.tick().await;
        for state in [
            RTCPeerConnectionState::Connected,
            RTCPeerConnectionState::Disconnected,
            RTCPeerConnectionState::Connected,
            RTCPeerConnectionState::Failed,
        ] {
            // Notification must survive a state change before the waiter starts.
            media.observe(state);
            tokio::time::timeout(Duration::from_millis(100), media.tick(&mut periodic))
                .await
                .expect("peer transition waited for the periodic status tick");
            assert_eq!(
                media.connected.load(Ordering::Acquire),
                state == RTCPeerConnectionState::Connected
            );
            assert_eq!(
                media.failed.load(Ordering::Acquire),
                terminal_peer_state(state)
            );
        }
    }

    #[tokio::test]
    async fn same_channel_join_reuses_the_whole_job_including_recovery_and_backoff() {
        let guild = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let channel = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        let other = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        let (stop, stopped) = oneshot::channel();
        let mut job = Job {
            guild_id: guild,
            channel_id: Arc::new(Mutex::new(channel)),
            stop,
            task: tokio::spawn(async {
                let _ = stopped.await;
            }),
        };
        // Job ownership doesn't depend on the UI's polled connected flag or
        // whether the task is negotiating, recovering audio/ICE, or in backoff.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            assert!(job.owns(guild, channel));
            assert!(!job.owns(guild, other));
        }
        *job.channel_id.lock().unwrap() = other;
        assert!(job.owns(guild, other));
        assert!(!job.owns(guild, channel));
        job.task.abort();
        let _ = (&mut job.task).await;
        assert!(!job.owns(guild, channel));
    }

    #[test]
    fn recoverable_peer_states_are_not_retry_triggers() {
        for state in [
            RTCPeerConnectionState::New,
            RTCPeerConnectionState::Connecting,
            RTCPeerConnectionState::Connected,
            RTCPeerConnectionState::Disconnected,
        ] {
            assert!(!terminal_peer_state(state));
        }
        assert!(terminal_peer_state(RTCPeerConnectionState::Failed));
        assert!(terminal_peer_state(RTCPeerConnectionState::Closed));
    }

    async fn signaling_endpoint() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let frame: ClientFrame = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(frame.version, VOICE_VERSION);
            assert!(matches!(frame.event, ClientEvent::Leave {}));
            assert!(socket.next().await.unwrap().unwrap().is_close());
        });
        (base, server)
    }

    #[tokio::test]
    async fn finished_attempt_sends_leave_and_close_after_receive_half_is_dropped() {
        let (base, server) = signaling_endpoint().await;
        let mut attempt = Attempt::default();
        let (send, receive) = crate::native_signaling::connect(&base)
            .await
            .unwrap()
            .split();
        attempt.signaling = Some(send);
        // run() has returned a failure, so its local receive half is gone.
        drop(receive);
        attempt.close_signaling().await;
        assert!(attempt.signaling.is_none());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancellation_preserves_signaling_for_explicit_cleanup() {
        struct Pending {
            attempt: Attempt,
            base: String,
            cancel: Option<oneshot::Sender<()>>,
        }
        impl Connector for Pending {
            async fn attempt(&mut self) -> (Failure, bool) {
                let (send, _receive) = crate::native_signaling::connect(&self.base)
                    .await
                    .unwrap()
                    .split();
                self.attempt.signaling = Some(send);
                self.cancel.take().unwrap().send(()).unwrap();
                std::future::pending().await
            }
            fn waiting(&mut self, _: &Failure, _: Duration) {
                panic!("cancelled connections must not retry");
            }
        }
        let (base, server) = signaling_endpoint().await;
        let (cancel, stopped) = oneshot::channel();
        let mut runtime = Pending {
            attempt: Attempt::default(),
            base,
            cancel: Some(cancel),
        };
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                reconnect::supervise(&mut runtime, stopped, || 0),
            )
            .await
            .unwrap()
            .is_none()
        );
        runtime.attempt.close_signaling().await;
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn policy_closes_are_final_but_server_restart_closes_retry() {
        use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
        for (code, retryable) in [
            (CloseCode::from(REPLACED_CLOSE_CODE), false),
            (CloseCode::Policy, false),
            (CloseCode::Protocol, false),
            (CloseCode::Restart, true),
            (CloseCode::Away, true),
        ] {
            let error = closed(&Message::Close(Some(CloseFrame {
                code,
                reason: "private-reason".into(),
            })));
            assert_eq!(error.retryable, retryable);
            assert!(!error.message.contains("private-reason"));
        }
    }
    #[test]
    fn dropping_attempt_cancels_pending_start_and_track_readers() {
        let attempt = Attempt::default();
        let queued_start = attempt.connection.clone();
        let closed = attempt.closed.subscribe();
        drop(attempt);
        assert!(!queued_start.active());
        assert!(*closed.borrow());
    }
}
