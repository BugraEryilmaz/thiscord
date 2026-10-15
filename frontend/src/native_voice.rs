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
    channel_id: ChannelId,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Job {
    fn owns(&self, guild: GuildId, channel: ChannelId) -> bool {
        self.guild_id == guild && self.channel_id == channel && !self.task.is_finished()
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
    failed: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    connection: Connection,
    closed: watch::Receiver<bool>,
    screen: crate::native_screen::ScreenState,
    keyframes: mpsc::Sender<(usize, u32)>,
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
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
        if terminal_peer_state(state) {
            self.failed.store(true, Ordering::Release);
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let ssrc = track.ssrcs().await.first().copied().unwrap_or(0);
        let screen_receiver = MediaKind::from_relay_ssrc(ssrc)
            .filter(|(kind, _)| *kind == MediaKind::ScreenVideo)
            .map(|(_, slot)| self.screen.receiver(self.connection.clone(), slot));
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
                        if let Some(epoch) = receiver.keyframe_request(Instant::now()) && let Some((_, slot)) = MediaKind::from_relay_ssrc(ssrc) { let _ = keyframes.try_send((slot, epoch)); }
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
                        engine.notify(Command::Packet {
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
    let task = tokio::spawn(async move {
        let mut runtime = Runtime {
            app: app.clone(),
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
        channel_id,
        stop,
        task,
    });
    Ok(())
}
struct Attempt {
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
        self.close().await;
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
        screen_feedback,
        screen_subscriptions,
        resume_token,
        remote_audio,
    ) = match first.event {
        ServerEvent::Offer {
            sdp,
            slot,
            can_speak,
            ice_servers,
            screen_video,
            screen_feedback,
            screen_subscriptions,
            resume_token,
            remote_audio,
        } if first.version == VOICE_VERSION && slot < ROOM_CAPACITY => (
            sdp,
            slot,
            can_speak,
            ice_servers,
            screen_video,
            screen_feedback,
            screen_subscriptions,
            resume_token,
            remote_audio,
        ),
        ServerEvent::Replaced {} => return Err(Failure::replaced()),
        ServerEvent::Error { error } => return Err(server_failure(&attempt.connection, error)),
        _ => return Err("Invalid voice offer".into()),
    };
    attempt.stage = "negotiate";
    let gathered = Arc::new(Notify::new());
    let failed = Arc::new(AtomicBool::new(false));
    let connected = Arc::new(AtomicBool::new(false));
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
    media.register_default_codecs().map_err(|e| e.to_string())?;
    let (keyframe_tx, mut keyframe_rx) = mpsc::channel(ROOM_CAPACITY);
    let force_keyframe = Arc::new(AtomicBool::new(false));
    let registry =
        register_default_interceptors(Registry::new(), &mut media).map_err(|e| e.to_string())?;
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
            .with_configuration(configuration)
            .with_interceptor_registry(registry)
            .with_media_engine(media)
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                engine: engine.clone(),
                failed: failed.clone(),
                connected: connected.clone(),
                connection: attempt.connection.clone(),
                closed: attempt.closed.subscribe(),
                keyframes: keyframe_tx,
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
    pc.set_remote_description(offer)
        .await
        .map_err(|_| "Cannot accept server offer")?;
    let track = thiscord_frontend::audio::transport::track(MediaKind::Microphone.publisher_ssrc());
    pc.add_track(track.clone() as Arc<dyn TrackLocal>)
        .await
        .map_err(|_| "Cannot add microphone track")?;
    let screen_track = thiscord_frontend::screen::track(MediaKind::ScreenVideo.publisher_ssrc());
    let system_track =
        thiscord_frontend::audio::transport::track(MediaKind::SystemAudio.publisher_ssrc());
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
    tokio::time::timeout(Duration::from_secs(8), gathered.notified())
        .await
        .map_err(|_| Failure::temporary("ICE gathering timed out"))?;
    let sdp = serde_json::to_string(&pc.local_description().await.ok_or("Missing answer")?)
        .map_err(|_| "Invalid answer")?;
    send_event(
        send,
        ClientEvent::Answer {
            remote_audio,
            sdp,
            screen_feedback,
            screen_subscriptions,
        },
        &attempt.connection,
    )
    .await?;
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
    let mut audio_started = false;
    let mut roster: Vec<Participant> = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let start = Instant::now();
    let mut screen_state = Some((false, false));
    let mut rtp_clock = thiscord_frontend::audio::outgoing::RtpClock::default();
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
        tokio::select! {
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
            Some((slot, epoch))=keyframe_rx.recv()=>{
                if screen_feedback && app.state::<crate::native_screen::ScreenState>().watching(slot, epoch) && roster.iter().any(|m| m.slot == slot && m.screen_epoch == epoch && m.sharing_screen) {
                    signaling.send(ClientEvent::ScreenKeyframe { slot, epoch });
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
                    ServerEvent::Resumed {} => {
                        screen_state = None;
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
                    ServerEvent::ScreenTarget { epoch, bitrate } => {
                        if status.lock().ok().is_some_and(|s| s.participants.iter().any(|m| m.slot == own_slot && m.screen_epoch == epoch && m.sharing_screen)) {
                            app.state::<crate::native_screen::ScreenState>().target(bitrate);
                        }
                    },
                    ServerEvent::MediaDiagnostics{counters}=>{ app.state::<crate::native_screen::ScreenState>().server_diagnostics(counters); },
                    ServerEvent::ScreenKeyframe{epoch}=>{
                        if status.lock().ok().is_some_and(|s| s.participants.iter().any(|m| m.slot == own_slot && m.screen_epoch == epoch && m.sharing_screen)) { force_keyframe.store(true, Ordering::Release); }
                    },
                    ServerEvent::Participants{members}=>{
                        if members.len()>ROOM_CAPACITY||members.iter().any(|m|m.slot>=ROOM_CAPACITY){return Err("Invalid participant list".into());}
                        app.state::<crate::native_screen::ScreenState>().roster(if settings.lock().map_err(|_|"Settings unavailable")?.deafened { &[] } else { &members });
                        roster = members.iter().filter(|m|m.slot!=own_slot).cloned().collect();
                        engine.notify(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()});
                        if let Ok(mut s)=status.lock(){s.participants=members;s.own_slot=Some(own_slot);}
                    },ServerEvent::Replaced{}=>return Err(Failure::replaced()),ServerEvent::Error{error}=>return Err(server_failure(&attempt.connection,error)),ServerEvent::Revoked{}=>return Err("Voice access changed. Join again if you still have permission.".into()),_=>{},
                }
            },
            Some(mut frame)=outgoing.recv()=>{
                if !connected.load(Ordering::Acquire) {
                    frame.finish(thiscord_frontend::audio::quality::Count::OutgoingFailedPackets);
                    continue;
                }
                if let Err(reason) = transmit_control.check(&frame, &attempt.connection, Instant::now()) {
                    transmit_control.dropped_frame(reason, &mut frame);
                    continue;
                }
                let (sequence, timestamp) = rtp_clock.pending(frame.media_position);
                let packet=rtc::rtp::Packet{header:rtc::rtp::header::Header{version:2,payload_type:MediaKind::Microphone.payload_type(),ssrc:MediaKind::Microphone.publisher_ssrc(),sequence_number:sequence,timestamp,marker:frame.marker,..Default::default()},payload:frame.payload.clone()};
                match transmit_control.guard_write(&frame, &attempt.connection, track.write_rtp(packet)).await {
                    Ok(result) => {
                        use thiscord_frontend::audio::quality::Count;
                        frame.finish(if result.is_ok() { Count::OutgoingSentPackets } else { Count::OutgoingFailedPackets });
                        result.map_err(|_|Failure::temporary("Voice media connection failed").with_reason("media_send_failed"))?;
                        rtp_clock.next(frame.media_position);
                        accepted_packets += 1;
                        accepted_bytes += frame.payload.len() as u64;
                    },
                    Err(reason) => transmit_control.dropped_frame(reason, &mut frame),
                }
            },
            _=ticker.tick()=>{
                if last_audio_stats.elapsed() >= Duration::from_secs(5) {
                    attempt.connection.event("voice_transmit", serde_json::json!({
                        "accepted_packets":accepted_packets,"accepted_payload_bytes":accepted_bytes,
                        "minimum_rtp_header_bytes":accepted_packets*12
                    }));
                    last_audio_stats = Instant::now();
                }
                if failed.load(Ordering::Acquire){
                    attempt.connection.event("voice_connection_lost",serde_json::json!({"peer_failed":failed.load(Ordering::Acquire)}));
                    return Err(Failure::temporary("Voice connection lost").with_reason("peer_failed"));
                }
                if !audio_started{
                    if start.elapsed()>Duration::from_secs(15){return Err(Failure::temporary("Media connection timed out").with_reason("media_connect_timeout"));}
                    if connected.load(Ordering::Acquire){
                        attempt.stage="audio_start";
                        let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();let audio_app=app.clone();let tx=tx.clone();let connection=attempt.connection.clone();
                        tauri::async_runtime::spawn_blocking(move||crate::native_audio::start_voice(&audio_app,guild_id,connection,configuration,tx,can_speak)).await.map_err(|_|"Audio task failed")??;
                        audio_started=true;attempt.connected_at=Some(Instant::now());attempt.stage="active";
                        attempt.connection.event("voice_media_connected",serde_json::json!({}));
                        if screen_video { app.state::<crate::native_screen::ScreenState>().bind(crate::native_screen::Binding {
                            connection: attempt.connection.clone(), video: screen_track.clone(), audio: system_track.clone(), can_publish: can_speak,
                            subscriptions: screen_subscriptions,
                            demand: Default::default(),
                            force_keyframe: force_keyframe.clone(), metrics: Default::default(),
                            video_sequence: Default::default(), audio_sequence: Default::default(), audio_timestamp: Default::default(), started: Instant::now(),
                        }); }
                        engine.notify(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()});
                        if let Ok(mut s)=status.lock(){s.connected=true;s.message="Connected".into();}
                    }
                }
                let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();
                if audio_started {
                    let e=engine.clone();
                    let heartbeat_connection=attempt.connection.clone();
                    let audio=tauri::async_runtime::spawn_blocking(move||e.command(Command::Keepalive(heartbeat_connection))).await.map_err(|_|"Audio worker failed")??;
                    if !audio.running {return Err(Failure::from(audio.message).with_reason("audio_stopped"));}
                    if let Ok(mut s)=status.lock(){s.message=audio.message;}
                    // Disconnected is recoverable by ICE itself. Keep this peer
                    // until Failed/Closed; signaling has its own bounded recovery.
                    if signaling.recovering() && let Ok(mut s)=status.lock() {
                        s.message="Recovering voice signaling...".into();
                    } else if !connected.load(Ordering::Acquire) && let Ok(mut s)=status.lock() {
                        s.message="Recovering voice connection...".into();
                    }
                }
                if configuration.deafened { crate::native_screen::screen_stop(app.state::<crate::native_screen::ScreenState>()); }
                let current_screen = app.state::<crate::native_screen::ScreenState>().sharing();
                if Some(current_screen) != screen_state {
                    signaling.send(ClientEvent::Screen { active: current_screen.0, audio: current_screen.1 });
                    screen_state = Some(current_screen);
                }
                signaling.send(ClientEvent::State{muted:configuration.muted,deafened:configuration.deafened});
                if screen_subscriptions {
                    let views = if configuration.deafened { vec![] } else { app.state::<crate::native_screen::ScreenState>().subscriptions() };
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
    async fn same_channel_join_reuses_the_whole_job_including_recovery_and_backoff() {
        let guild = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let channel = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        let other = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        let (stop, stopped) = oneshot::channel();
        let mut job = Job {
            guild_id: guild,
            channel_id: channel,
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
