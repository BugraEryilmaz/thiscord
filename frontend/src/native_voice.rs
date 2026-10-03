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

type Job = (oneshot::Sender<()>, tokio::task::JoinHandle<()>);
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
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
        if matches!(
            state,
            RTCPeerConnectionState::Closed | RTCPeerConnectionState::Failed
        ) {
            self.failed.store(true, Ordering::Release);
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let engine = self.engine.clone();
        let connection = self.connection.clone();
        let mut closed = self.closed.clone();
        tokio::spawn(async move {
            loop {
                if !connection.active() {
                    break;
                }
                let event = tokio::select! { biased;
                    _ = closed.changed() => break,
                    event = track.poll() => match event { Some(event) => event, None => break },
                };
                if let TrackRemoteEvent::OnRtpPacket(packet) = event
                    && let Some(slot) = packet
                        .header
                        .ssrc
                        .checked_sub(SSRC_BASE)
                        .filter(|s| (*s as usize) < ROOM_CAPACITY)
                {
                    engine.notify(Command::Packet {
                        connection: connection.clone(),
                        slot: slot as usize,
                        sequence: packet.header.sequence_number,
                        payload: packet.payload,
                    });
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
    if let Some((stop, mut task)) = guard.take() {
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
    crate::native_audio::unregister_ptt(&app);
    if let Ok(mut status) = state.status.lock() {
        *status = VoiceStatus {
            message: "Disconnected from voice".into(),
            ..Default::default()
        };
    }
    Ok(())
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
    voice_leave(app.clone(), state.clone()).await?;
    let mut job = state.job.lock().await;
    if job.is_some() {
        return Err("A voice connection is already starting".into());
    }
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
    *job = Some((stop, task));
    Ok(())
}
struct Attempt {
    peer: Option<Arc<dyn PeerConnection>>,
    connection: Connection,
    closed: watch::Sender<bool>,
    connected_at: Option<Instant>,
}
impl Default for Attempt {
    fn default() -> Self {
        Self {
            peer: None,
            connection: Connection::default(),
            closed: watch::channel(false).0,
            connected_at: None,
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
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
        self.attempt.connection.close();
        self.attempt.closed.send_replace(true);
        self.engine.notify(Command::Stop);
        self.engine.notify(Command::Pressed(false));
        if let Some(peer) = &self.attempt.peer {
            let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
        }
        self.attempt.peer = None;
    }
}
impl Connector for Runtime {
    async fn attempt(&mut self) -> (Failure, bool) {
        self.attempt = Attempt::default();
        let configuration = self.settings.lock().map(|s| s.clone());
        let registration = configuration
            .map_err(|_| "Settings unavailable".to_string())
            .and_then(|s| crate::native_audio::register_ptt(&self.app, &s));
        if let Err(error) = registration {
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
async fn send_event<S: futures_util::Sink<Message> + Unpin>(
    send: &mut S,
    event: ClientEvent,
) -> Result<(), Failure> {
    tokio::time::timeout(Duration::from_secs(3), send.send(frame(event)?))
        .await
        .map_err(|_| Failure::temporary("Voice signaling sender stalled"))?
        .map_err(|_| Failure::temporary("Voice socket closed"))
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
fn closed(message: &Message) -> Failure {
    if let Message::Close(Some(frame)) = message {
        let code = u16::from(frame.code);
        if !matches!(code, 1000 | 1001 | 1006 | 1011..=1014) {
            // Policy/protocol closes are final; never display arbitrary close bodies.
            return Failure::from("Voice connection rejected or protocol unsupported");
        }
    }
    Failure::temporary("Voice connection closed")
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
    let ws = crate::native_signaling::connect(base).await?;
    let (mut send, mut receive) = ws.split();
    send_event(
        &mut send,
        ClientEvent::Join {
            token: token.into(),
            guild_id,
            channel_id,
        },
    )
    .await?;
    let first = tokio::time::timeout(Duration::from_secs(20), receive.next())
        .await
        .map_err(|_| Failure::temporary("Voice offer timed out"))?
        .ok_or_else(|| Failure::temporary("Voice socket closed"))?
        .map_err(|_| Failure::temporary("Voice socket failed"))?;
    if first.is_close() {
        return Err(closed(&first));
    }
    let first: ServerFrame = serde_json::from_str(first.to_text().map_err(|_| "Invalid offer")?)
        .map_err(|_| "Invalid offer")?;
    if first.version != VOICE_VERSION {
        return Err("Unsupported voice version".into());
    }
    let (sdp, own_slot, can_speak, ice_servers) = match first.event {
        ServerEvent::Offer {
            sdp,
            slot,
            can_speak,
            ice_servers,
        } if first.version == VOICE_VERSION && slot < ROOM_CAPACITY => {
            (sdp, slot, can_speak, ice_servers)
        }
        ServerEvent::Error { error } => return Err(Failure::server(error)),
        _ => return Err("Invalid voice offer".into()),
    };
    let gathered = Arc::new(Notify::new());
    let failed = Arc::new(AtomicBool::new(false));
    let connected = Arc::new(AtomicBool::new(false));
    let mut media = MediaEngine::default();
    media.register_default_codecs().map_err(|e| e.to_string())?;
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
            }))
            .with_udp_addrs(vec![bind().map_err(Failure::temporary)?])
            .build()
            .await
            .map_err(|_| Failure::temporary("Cannot initialize voice media connection"))?,
    );
    attempt.peer = Some(pc.clone());
    let offer: RTCSessionDescription = serde_json::from_str(&sdp).map_err(|_| "Invalid SDP")?;
    pc.set_remote_description(offer)
        .await
        .map_err(|_| "Cannot accept server offer")?;
    let track = thiscord_frontend::audio::transport::track(900);
    pc.add_track(track.clone() as Arc<dyn TrackLocal>)
        .await
        .map_err(|_| "Cannot add microphone track")?;
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
    send_event(&mut send, ClientEvent::Answer { sdp }).await?;
    let (tx, mut outgoing) = mpsc::channel(5);
    let mut audio_started = false;
    let mut disconnected_since: Option<Instant> = None;
    let mut roster = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let start = Instant::now();
    let mut heard = Instant::now();
    let mut sequence = 0_u16;
    let mut timestamp = 0_u32;
    loop {
        tokio::select! {
            message=receive.next()=>{
                let message=message.ok_or_else(||Failure::temporary("Voice connection closed"))?.map_err(|_|Failure::temporary("Voice socket failed"))?;
                if message.is_close(){return Err(closed(&message));}
                if !message.is_text(){continue;}
                let message:ServerFrame=serde_json::from_str(message.to_text().map_err(|_|"Invalid voice event")?).map_err(|_|"Invalid voice event")?;
                if message.version!=VOICE_VERSION{return Err("Unsupported voice version".into());}heard=Instant::now();
                match message.event{
                    ServerEvent::Participants{members}=>{
                        if members.len()>ROOM_CAPACITY||members.iter().any(|m|m.slot>=ROOM_CAPACITY){return Err("Invalid participant list".into());}
                        roster = members.iter().filter(|m|m.slot!=own_slot).cloned().collect();
                        engine.notify(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()});
                        if let Ok(mut s)=status.lock(){s.participants=members;}
                    },ServerEvent::Error{error}=>return Err(Failure::server(error)),ServerEvent::Revoked{}=>return Err("Voice access changed. Join again if you still have permission.".into()),_=>{},
                }
            },
            Some(payload)=outgoing.recv()=>{
                sequence=sequence.wrapping_add(1);timestamp=timestamp.wrapping_add(960);let packet=rtc::rtp::Packet{header:rtc::rtp::header::Header{version:2,payload_type:111,ssrc:900,sequence_number:sequence,timestamp,..Default::default()},payload};
                tokio::time::timeout(Duration::from_millis(100),track.write_rtp(packet)).await.map_err(|_|Failure::temporary("Voice sender stalled"))?.map_err(|_|Failure::temporary("Voice media connection failed"))?;
            },
            _=ticker.tick()=>{
                if failed.load(Ordering::Acquire)||heard.elapsed()>Duration::from_secs(20){return Err(Failure::temporary("Voice connection lost"));}
                if !audio_started{
                    if start.elapsed()>Duration::from_secs(15){return Err(Failure::temporary("Media connection timed out"));}
                    if connected.load(Ordering::Acquire){
                        let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();let app=app.clone();let tx=tx.clone();let connection=attempt.connection.clone();
                        tauri::async_runtime::spawn_blocking(move||crate::native_audio::start_voice(&app,guild_id,connection,configuration,tx,can_speak)).await.map_err(|_|"Audio task failed")??;
                        audio_started=true;attempt.connected_at=Some(Instant::now());
                        engine.notify(Command::Roster{connection:attempt.connection.clone(),members:roster.clone()});
                        if let Ok(mut s)=status.lock(){s.connected=true;s.message="Connected".into();}
                    }
                }
                let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();
                if audio_started {
                    let e=engine.clone();
                    let audio=tauri::async_runtime::spawn_blocking(move||e.command(Command::Peek)).await.map_err(|_|"Audio worker failed")??;
                    if !audio.running {return Err(audio.message.into());}
                    if connected.load(Ordering::Acquire) { disconnected_since=None; }
                    else if disconnected_since.get_or_insert_with(Instant::now).elapsed()>Duration::from_secs(5) {return Err(Failure::temporary("Voice media connection interrupted"));}
                }
                send_event(&mut send, ClientEvent::State{muted:configuration.muted,deafened:configuration.deafened}).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_closes_are_final_but_server_restart_closes_retry() {
        use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
        for (code, retryable) in [
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
