//! Bounded, single-process audio SFU. One WebRTC transport per participant.
mod ice;
mod store;
use crate::{auth::Failure, chat, db::DbPool};
use axum::{
    Extension, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
};
use rtc::{
    media_stream::MediaStreamTrack,
    rtp,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
    },
};
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::{ChannelId, GuildId, RequestId, voice::*};
use tokio::sync::{Mutex, Notify, RwLock, Semaphore, mpsc, watch};
use webrtc::{
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

#[derive(Clone)]
struct Member {
    info: Participant,
    tx: mpsc::Sender<(u64, usize, uuid::Uuid, rtp::Packet)>,
    active: Arc<AtomicBool>,
}
#[derive(Default)]
struct Room {
    members: RwLock<HashMap<usize, Member>>,
}
struct VoiceAccess {
    pool: DbPool,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    generation: Arc<AtomicU64>,
    active: Arc<AtomicBool>,
}
impl VoiceAccess {
    async fn refresh(
        &self,
        room: &Room,
        slot: usize,
        changed: &mut watch::Receiver<u64>,
    ) -> Result<Vec<Participant>, Failure> {
        // Serialize authorization with access-changing commits. Media remains
        // blocked at both ends until this participant validates the new epoch.
        let _gate = chat::gate().write().await;
        let pool = self.pool.clone();
        let token = self.token.clone();
        let (guild, channel) = (self.guild, self.channel);
        let result =
            tokio::task::spawn_blocking(move || store::authorize(&pool, &token, guild, channel))
                .await
                .unwrap_or(Err(Failure::Unavailable));
        let mut members = room.members.write().await;
        let result = result.and_then(|info| {
            let member = members.get_mut(&slot).ok_or(Failure::Forbidden)?;
            // Speak controls native microphone setup in the negotiated offer.
            // A changed grant requires a fresh join; unrelated grants do not.
            if info.account_id != member.info.account_id || info.can_speak != member.info.can_speak
            {
                return Err(Failure::Forbidden);
            }
            member.info.username = info.username;
            self.generation
                .store(*changed.borrow_and_update(), Ordering::Release);
            Ok(members.values().map(|m| m.info.clone()).collect())
        });
        if result.is_err() {
            self.active.store(false, Ordering::Release);
        }
        result
    }
}
async fn room(id: ChannelId) -> Arc<Room> {
    static ROOMS: OnceLock<Mutex<HashMap<ChannelId, Weak<Room>>>> = OnceLock::new();
    let mut rooms = ROOMS.get_or_init(Default::default).lock().await;
    rooms.retain(|_, r| r.strong_count() > 0);
    if let Some(room) = rooms.get(&id).and_then(Weak::upgrade) {
        return room;
    }
    let room = Arc::new(Room::default());
    rooms.insert(id, Arc::downgrade(&room));
    room
}
pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    Router::new()
        .route(VOICE_PATH, get(upgrade))
        .layer(Extension(Arc::new(origins)))
}
async fn upgrade(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(origins): Extension<Arc<Vec<HeaderValue>>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !headers.get("origin").is_some_and(|v| origins.contains(v)) {
        return Failure::Forbidden.response(id);
    }
    let Some(pool) = pool else {
        return Failure::Unavailable.response(id);
    };
    let Ok(ws) = ws else {
        return Failure::Invalid("Invalid voice socket upgrade").response(id);
    };
    static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let Ok(permit) = LIMIT
        .get_or_init(|| Arc::new(Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
    else {
        return Failure::Limited.response(id);
    };
    ws.max_message_size(128 * 1024)
        .max_frame_size(128 * 1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            serve(socket, pool, id).await
        })
        .into_response()
}
async fn send(socket: &mut WebSocket, event: ServerEvent) -> Result<(), ()> {
    let text = serde_json::to_string(&ServerFrame {
        version: VOICE_VERSION,
        event,
    })
    .map_err(|_| ())?;
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
fn parse(message: Message) -> Option<ClientEvent> {
    let Message::Text(text) = message else {
        return None;
    };
    let frame: ClientFrame = serde_json::from_str(&text).ok()?;
    (frame.version == VOICE_VERSION).then_some(frame.event)
}
async fn fail(socket: &mut WebSocket, id: RequestId, failure: Failure) {
    let response = failure.response(id);
    if let Ok(body) = axum::body::to_bytes(response.into_body(), 8192).await
        && let Ok(error) = serde_json::from_slice(&body)
    {
        let _ = send(socket, ServerEvent::Error { error }).await;
    }
}
async fn access_failed(socket: &mut WebSocket, id: RequestId, failure: Failure) {
    match failure {
        Failure::Unauthorized | Failure::Forbidden => {
            tracing::info!(request_id = %id, "voice session or channel access revoked");
            let _ = send(socket, ServerEvent::Revoked {}).await;
        }
        error => {
            tracing::warn!(request_id = %id, "voice authorization check failed");
            fail(socket, id, error).await;
        }
    }
}
struct Handler {
    gathered: Arc<Notify>,
    packets: mpsc::Sender<(u64, rtp::Packet)>,
    active: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    track_seen: AtomicBool,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        if matches!(
            s,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.active.store(false, Ordering::Release);
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let Some(ssrc) = track.ssrcs().await.first().copied() else {
            self.active.store(false, Ordering::Release);
            return;
        };
        if !track.codec(ssrc).await.is_some_and(|c| {
            c.mime_type.eq_ignore_ascii_case("audio/opus") && c.clock_rate == 48_000
        }) {
            self.active.store(false, Ordering::Release);
            return;
        }
        if self.track_seen.swap(true, Ordering::AcqRel) {
            self.active.store(false, Ordering::Release);
            return;
        }
        let tx = self.packets.clone();
        let active = self.active.clone();
        let generation = self.generation.clone();
        tokio::spawn(async move {
            let mut since = Instant::now();
            let mut count = 0;
            while let Some(event) = track.poll().await {
                if !active.load(Ordering::Acquire) {
                    break;
                }
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    if since.elapsed() > Duration::from_secs(1) {
                        since = Instant::now();
                        count = 0;
                    }
                    count += 1;
                    if count > 100 || packet.payload.len() > 1500 {
                        active.store(false, Ordering::Release);
                        break;
                    }
                    let _ = tx.try_send((generation.load(Ordering::Acquire), packet));
                }
            }
        });
    }
}
fn local_bind() -> Result<String, String> {
    if let Ok(bind) = std::env::var("THISCORD_VOICE_BIND") {
        return Ok(bind);
    }
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket.connect("192.0.2.1:9").map_err(|e| e.to_string())?;
    Ok(format!(
        "{}:0",
        socket.local_addr().map_err(|e| e.to_string())?.ip()
    ))
}
fn local_track(slot: usize) -> Arc<TrackLocalStaticRTP> {
    Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("speaker-{slot}"),
        format!("speaker-{slot}"),
        format!("speaker-{slot}"),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(SSRC_BASE + slot as u32),
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
    )))
}
async fn log_selected_route(pc: &dyn PeerConnection, id: RequestId) -> bool {
    for sender in pc.get_senders().await {
        if let Ok(Some(dtls)) = sender.transport().await
            && let Ok(Some(pair)) = dtls.ice_transport().get_selected_candidate_pair().await
        {
            // Candidate types explain direct versus TURN routing without logging
            // addresses, SDP, ICE credentials or packet contents.
            tracing::info!(
                request_id = %id,
                local_candidate_type = %pair.local().typ,
                remote_candidate_type = %pair.remote().typ,
                "voice media route selected"
            );
            return true;
        }
    }
    false
}
async fn serve(mut socket: WebSocket, pool: DbPool, id: RequestId) {
    let mut changed = chat::changes().subscribe();
    let Ok(Some(Ok(first))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Some(ClientEvent::Join {
        token,
        guild_id,
        channel_id,
    }) = parse(first)
    else {
        fail(
            &mut socket,
            id,
            Failure::Invalid("Join voice with version 1 first"),
        )
        .await;
        return;
    };
    let access = chat::gate().write().await;
    let p = pool.clone();
    let t = token.clone();
    let authorized = tokio::task::spawn_blocking(move || {
        let info = store::authorize(&p, &t, guild_id, channel_id)?;
        crate::auth::store::rate_limit(
            &p,
            &format!("voice:{}", info.account_id),
            Some(&format!("voice:{}", info.account_id)),
            true,
        )?;
        Ok::<_, Failure>(info)
    })
    .await;
    let mut info = match authorized {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => {
            fail(&mut socket, id, error).await;
            return;
        }
        Err(_) => {
            fail(&mut socket, id, Failure::Unavailable).await;
            return;
        }
    };
    let generation = Arc::new(AtomicU64::new(*changed.borrow_and_update()));
    let ice_servers = match ice::servers(info.account_id) {
        Ok(servers) => servers,
        Err(_) => {
            fail(&mut socket, id, Failure::Unavailable).await;
            return;
        }
    };
    let room = room(channel_id).await;
    let active = Arc::new(AtomicBool::new(true));
    let (out_tx, mut out_rx) = mpsc::channel::<(u64, usize, uuid::Uuid, rtp::Packet)>(32);
    let source_id = uuid::Uuid::new_v4();
    let slot = {
        let mut members = room.members.write().await;
        if members
            .values()
            .any(|m| m.info.account_id == info.account_id)
        {
            fail(
                &mut socket,
                id,
                Failure::Invalid("Already connected to this voice channel"),
            )
            .await;
            return;
        }
        let Some(slot) = (0..ROOM_CAPACITY).find(|s| !members.contains_key(s)) else {
            fail(&mut socket, id, Failure::Limited).await;
            return;
        };
        info.slot = slot;
        members.insert(
            slot,
            Member {
                info: info.clone(),
                tx: out_tx,
                active: active.clone(),
            },
        );
        slot
    };
    drop(access);
    let gathered = Arc::new(Notify::new());
    let (in_tx, mut in_rx) = mpsc::channel::<(u64, rtp::Packet)>(8);
    let mut engine = MediaEngine::default();
    let _ = engine.register_default_codecs();
    let registry = register_default_interceptors(Registry::new(), &mut engine)
        .expect("default RTP interceptors");
    let built = PeerConnectionBuilder::new()
        .with_configuration(ice::configuration(&ice_servers))
        .with_interceptor_registry(registry)
        .with_media_engine(engine)
        .with_handler(Arc::new(Handler {
            gathered: gathered.clone(),
            packets: in_tx,
            active: active.clone(),
            generation: generation.clone(),
            track_seen: AtomicBool::new(false),
        }))
        .with_udp_addrs(vec![local_bind().unwrap_or_else(|_| "127.0.0.1:0".into())])
        .build()
        .await;
    let Ok(pc) = built else {
        active.store(false, Ordering::Release);
        room.members.write().await.remove(&slot);
        fail(&mut socket, id, Failure::Unavailable).await;
        return;
    };
    let pc: Arc<dyn PeerConnection> = Arc::new(pc);
    let mut tracks = Vec::new();
    let negotiated = tokio::time::timeout(Duration::from_secs(15), async {
        for slot in 0..ROOM_CAPACITY {
            let track = local_track(slot);
            pc.add_track(track.clone() as Arc<dyn TrackLocal>)
                .await
                .map_err(|_| ())?;
            tracks.push(track);
        }
        let offer = pc.create_offer(None).await.map_err(|_| ())?;
        pc.set_local_description(offer).await.map_err(|_| ())?;
        gathered.notified().await;
        let sdp =
            serde_json::to_string(&pc.local_description().await.ok_or(())?).map_err(|_| ())?;
        send(
            &mut socket,
            ServerEvent::Offer {
                sdp,
                slot,
                can_speak: info.can_speak,
                ice_servers,
            },
        )
        .await?;
        let Some(Ok(answer)) = socket.recv().await else {
            return Err(());
        };
        let Some(ClientEvent::Answer { sdp }) = parse(answer) else {
            return Err(());
        };
        let answer: RTCSessionDescription = serde_json::from_str(&sdp).map_err(|_| ())?;
        if answer.sdp_type != RTCSdpType::Answer {
            return Err(());
        }
        pc.set_remote_description(answer).await.map_err(|_| ())?;
        Ok(())
    })
    .await;
    if matches!(negotiated, Ok(Ok(()))) {
        let relay_room = room.clone();
        let relay_active = active.clone();
        let relay_generation = generation.clone();
        let relay = tokio::spawn(async move {
            while let Some((epoch, mut packet)) = in_rx.recv().await {
                let _gate = chat::gate().read().await;
                if !relay_active.load(Ordering::Acquire) {
                    break;
                }
                let current = *chat::changes().borrow();
                if relay_generation.load(Ordering::Acquire) != current || epoch != current {
                    continue;
                }
                let members = relay_room.members.read().await;
                let Some(source) = members.get(&slot) else {
                    break;
                };
                if !source.info.can_speak || source.info.muted || source.info.deafened {
                    continue;
                }
                packet.header = rtp::header::Header {
                    version: 2,
                    payload_type: 111,
                    ssrc: SSRC_BASE + slot as u32,
                    sequence_number: packet.header.sequence_number,
                    timestamp: packet.header.timestamp,
                    ..Default::default()
                };
                for (&other, m) in members.iter() {
                    if other != slot && !m.info.deafened && m.active.load(Ordering::Acquire) {
                        let _ = m.tx.try_send((epoch, slot, source_id, packet.clone()));
                    }
                }
            }
        });
        let writer_active = active.clone();
        let writer_generation = generation.clone();
        let writer = tokio::spawn(async move {
            let mut sources = [None; ROOM_CAPACITY];
            let mut offsets = [(0_u16, 0_u32); ROOM_CAPACITY];
            let mut last = [(0_u16, 0_u32); ROOM_CAPACITY];
            while let Some((epoch, slot, source, mut packet)) = out_rx.recv().await {
                let _gate = chat::gate().read().await;
                if !writer_active.load(Ordering::Acquire) {
                    break;
                }
                let current = *chat::changes().borrow();
                if writer_generation.load(Ordering::Acquire) != current || epoch != current {
                    continue;
                }
                // A slot may be reused, but its SRTP sequence must not rewind.
                // Preserve sequence gaps/reordering within the publisher stream.
                if sources[slot] != Some(source) {
                    sources[slot] = Some(source);
                    offsets[slot] = (
                        last[slot]
                            .0
                            .wrapping_add(1)
                            .wrapping_sub(packet.header.sequence_number),
                        last[slot]
                            .1
                            .wrapping_add(960)
                            .wrapping_sub(packet.header.timestamp),
                    );
                }
                packet.header.sequence_number =
                    packet.header.sequence_number.wrapping_add(offsets[slot].0);
                packet.header.timestamp = packet.header.timestamp.wrapping_add(offsets[slot].1);
                if packet.header.sequence_number.wrapping_sub(last[slot].0) as i16 > 0 {
                    last[slot] = (packet.header.sequence_number, packet.header.timestamp);
                }
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_millis(100),
                        tracks[slot].write_rtp(packet)
                    )
                    .await,
                    Ok(Ok(_))
                ) {
                    writer_active.store(false, Ordering::Release);
                    break;
                }
            }
        });
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let access = VoiceAccess {
            pool,
            token,
            guild: guild_id,
            channel: channel_id,
            generation,
            active: active.clone(),
        };
        let mut last = Instant::now();
        let mut budget = 0;
        let mut route_logged = false;
        loop {
            tokio::select! {biased;
                result=changed.changed()=>{
                    if result.is_err(){break;}
                    if let Err(error)=access.refresh(&room,slot,&mut changed).await {
                        access_failed(&mut socket,id,error).await;
                        break;
                    }
                },
                message=socket.recv()=>{
                    let Some(Ok(message))=message else{break;};budget+=1;if budget>30{break;}last=Instant::now();
                    match parse(message){
                        Some(ClientEvent::Ping{})=>{if send(&mut socket,ServerEvent::Pong{}).await.is_err(){break;}},
                        Some(ClientEvent::State{muted,deafened})=>{if let Some(m)=room.members.write().await.get_mut(&slot){m.info.muted=muted;m.info.deafened=deafened;}},
                        Some(ClientEvent::Leave{})=>break,_=>break,
                    }
                },
                _=tick.tick()=>{
                    budget=0;if last.elapsed()>Duration::from_secs(20)||!active.load(Ordering::Acquire){break;}
                    let members=match access.refresh(&room,slot,&mut changed).await {
                        Ok(members)=>members,
                        Err(error)=>{access_failed(&mut socket,id,error).await;break;}
                    };
                    if !route_logged { route_logged=log_selected_route(pc.as_ref(),id).await; }
                if send(&mut socket,ServerEvent::Participants{members}).await.is_err(){break;}
                }
            }
        }
        active.store(false, Ordering::Release);
        relay.abort();
        writer.abort();
        let _ = relay.await;
        let _ = writer.await;
    }
    active.store(false, Ordering::Release);
    room.members.write().await.remove(&slot);
    let _ = tokio::time::timeout(Duration::from_secs(2), pc.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}
