use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, State};
use thiscord_frontend::audio::{AudioEngine, Command};
use thiscord_shared::{ChannelId, GuildId, audio::AudioSettings, voice::*};
use tokio::sync::{Notify, mpsc, oneshot};
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
        tokio::spawn(async move {
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(packet) = event
                    && let Some(slot) = packet
                        .header
                        .ssrc
                        .checked_sub(SSRC_BASE)
                        .filter(|s| (*s as usize) < ROOM_CAPACITY)
                {
                    engine.notify(Command::Packet {
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
        }
    }
    app.state::<crate::native_audio::AudioState>()
        .engine
        .notify(Command::Stop);
    crate::native_audio::unregister_ptt(&app);
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
    let (stop, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut peer: Option<Arc<dyn PeerConnection>> = None;
        let result = tokio::select! {biased;_= &mut stopped=>Ok(()),result=run(&token,guild_id,channel_id,engine.clone(),configuration,status.clone(),&mut peer)=>result};
        engine.notify(Command::Stop);
        if let Some(peer) = peer {
            let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
        }
        crate::native_audio::unregister_ptt(&app);
        if let Ok(mut s) = status.lock() {
            *s = VoiceStatus {
                message: result
                    .err()
                    .unwrap_or_else(|| "Disconnected from voice".into()),
                ..Default::default()
            };
        }
    });
    *job = Some((stop, task));
    Ok(())
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
fn bind() -> Result<String, String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.connect("192.0.2.1:9").map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:0",
        s.local_addr().map_err(|e| e.to_string())?.ip()
    ))
}
async fn run(
    token: &str,
    guild_id: GuildId,
    channel_id: ChannelId,
    engine: AudioEngine,
    settings: Arc<Mutex<AudioSettings>>,
    status: Arc<Mutex<VoiceStatus>>,
    peer: &mut Option<Arc<dyn PeerConnection>>,
) -> Result<(), String> {
    let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
    let ws = crate::native_signaling::connect(base).await?;
    let (mut send, mut receive) = ws.split();
    send.send(frame(ClientEvent::Join {
        token: token.into(),
        guild_id,
        channel_id,
    })?)
    .await
    .map_err(|_| "Voice socket closed")?;
    let first = tokio::time::timeout(Duration::from_secs(20), receive.next())
        .await
        .map_err(|_| "Voice offer timed out")?
        .ok_or("Voice socket closed")?
        .map_err(|_| "Voice socket failed")?;
    let first: ServerFrame = serde_json::from_str(first.to_text().map_err(|_| "Invalid offer")?)
        .map_err(|_| "Invalid offer")?;
    let (sdp, own_slot, can_speak, ice_servers) = match first.event {
        ServerEvent::Offer {
            sdp,
            slot,
            can_speak,
            ice_servers,
        } if first.version == VOICE_VERSION && slot < ROOM_CAPACITY => {
            (sdp, slot, can_speak, ice_servers)
        }
        ServerEvent::Error { error } => return Err(error.message),
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
            }))
            .with_udp_addrs(vec![bind()?])
            .build()
            .await
            .map_err(|e| e.to_string())?,
    );
    *peer = Some(pc.clone());
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
        .map_err(|_| "ICE gathering timed out")?;
    let sdp = serde_json::to_string(&pc.local_description().await.ok_or("Missing answer")?)
        .map_err(|_| "Invalid answer")?;
    send.send(frame(ClientEvent::Answer { sdp })?)
        .await
        .map_err(|_| "Voice socket closed")?;
    let (tx, mut outgoing) = mpsc::channel(5);
    let mut audio_started = false;
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    let start = Instant::now();
    let mut heard = Instant::now();
    let mut sequence = 0_u16;
    let mut timestamp = 0_u32;
    loop {
        tokio::select! {
            message=receive.next()=>{
                let message=message.ok_or("Voice connection closed")?.map_err(|_|"Voice socket failed")?;
                if message.is_close(){return Err("Voice connection closed".into());}
                if !message.is_text(){continue;}
                let message:ServerFrame=serde_json::from_str(message.to_text().map_err(|_|"Invalid voice event")?).map_err(|_|"Invalid voice event")?;
                if message.version!=VOICE_VERSION{return Err("Unsupported voice version".into());}heard=Instant::now();
                match message.event{
                    ServerEvent::Participants{members}=>{
                        if members.len()>ROOM_CAPACITY||members.iter().any(|m|m.slot>=ROOM_CAPACITY){return Err("Invalid participant list".into());}
                        engine.notify(Command::Roster(members.iter().filter(|m|m.slot!=own_slot).cloned().collect()));
                        if let Ok(mut s)=status.lock(){s.participants=members;}
                    },ServerEvent::Error{error}=>return Err(error.message),ServerEvent::Revoked{}=>return Err("Voice access changed. Join again if you still have permission.".into()),_=>{},
                }
            },
            Some(payload)=outgoing.recv()=>{
                sequence=sequence.wrapping_add(1);timestamp=timestamp.wrapping_add(960);let packet=rtc::rtp::Packet{header:rtc::rtp::header::Header{version:2,payload_type:111,ssrc:900,sequence_number:sequence,timestamp,..Default::default()},payload};
                tokio::time::timeout(Duration::from_millis(100),track.write_rtp(packet)).await.map_err(|_|"Voice sender stalled")?.map_err(|_|"Voice media connection failed")?;
            },
            _=ticker.tick()=>{
                if failed.load(Ordering::Acquire)||heard.elapsed()>Duration::from_secs(20){return Err("Voice connection lost".into());}
                if !audio_started{
                    if start.elapsed()>Duration::from_secs(15){return Err("Media connection timed out. Check WSL UDP routing, firewall and STUN/TURN configuration.".into());}
                    if connected.load(Ordering::Acquire){
                        let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();let e=engine.clone();let tx=tx.clone();
                        tauri::async_runtime::spawn_blocking(move||e.command(Command::VoiceStart{settings:configuration,outgoing:tx,microphone:can_speak})).await.map_err(|_|"Audio task failed")??;
                        audio_started=true;if let Ok(mut s)=status.lock(){s.connected=true;s.message="Connected".into();}
                    }
                }
                let configuration=settings.lock().map_err(|_|"Settings unavailable")?.clone();
                if audio_started {
                    let e=engine.clone();
                    let audio=tauri::async_runtime::spawn_blocking(move||e.command(Command::Peek)).await.map_err(|_|"Audio worker failed")??;
                    if !audio.running {return Err(audio.message);}
                }
                send.send(frame(ClientEvent::State{muted:configuration.muted,deafened:configuration.deafened})?).await.map_err(|_|"Voice socket closed")?;
            }
        }
    }
}
