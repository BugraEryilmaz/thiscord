//! Bounded, single-process audio SFU. One WebRTC transport per participant.
use crate::access;
mod diagnostics;
mod egress;
mod ice;
mod logging;
mod membership;
mod resumption;
mod store;
mod viewing;
use crate::{auth::Failure, db::DbPool};
use axum::{
    Extension, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, rejection::WebSocketUpgradeRejection},
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
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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
    resumption: Option<resumption::Handle>,
    request_id: RequestId,
    call_id: Option<RequestId>,
    replaced: watch::Sender<bool>,
    network: Arc<RwLock<thiscord_shared::admin::NetworkMetrics>>,
    info: Participant,
    source_id: uuid::Uuid,
    screen_started: Instant,
    camera_started: Instant,
    outgoing: [egress::Sender; 4],
    keyframes: mpsc::Sender<(MediaKind, u32)>,
    last_keyframe: [Option<Instant>; 2],
    feedback_enabled: bool,
    subscriptions_enabled: bool,
    views: [Option<viewing::View>; ROOM_CAPACITY * 2],
    generation: Arc<AtomicU64>,
    access: Arc<access::Access>,
    metrics: Arc<[AtomicU64; 5]>,
    active: Arc<AtomicBool>,
}
struct Delivery {
    epoch: u64,
    receiver_epoch: u64,
    source_access: Arc<access::Access>,
    slot: usize,
    source: uuid::Uuid,
    source_active: Arc<AtomicBool>,
    packet: rtp::Packet,
    queued_at: Instant,
}
#[derive(Default)]
struct Room {
    notification_target: Option<(GuildId, ChannelId)>,
    screen_epoch: AtomicU32,
    members: RwLock<HashMap<usize, Member>>,
}
struct VoiceAccess {
    access: Arc<access::Access>,
    pool: DbPool,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    voice_revision: i64,
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
        let result = self
            .access
            .authorize(changed, || {
                let pool = self.pool.clone();
                let token = self.token.clone();
                let (guild, channel) = (self.guild, self.channel);
                async move {
                    tokio::task::spawn_blocking(move || {
                        store::authorize(&pool, &token, guild, channel)
                    })
                    .await
                    .unwrap_or(Err(Failure::Unavailable))
                }
            })
            .await;
        let mut members = room.members.write().await;
        let result = result.and_then(|((info, voice_revision), epoch)| {
            if !self.active.load(Ordering::Acquire) {
                return Err(Failure::Forbidden);
            }
            let member = members.get_mut(&slot).ok_or(Failure::Forbidden)?;
            // Speak controls native microphone setup in the negotiated offer.
            // A changed grant requires a fresh join; unrelated grants do not.
            if voice_revision != self.voice_revision
                || info.account_id != member.info.account_id
                || info.can_speak != member.info.can_speak
            {
                return Err(Failure::Forbidden);
            }
            member.info.username = info.username;
            member.info.display_name = info.display_name;
            member.info.avatar_id = info.avatar_id;
            self.generation.store(epoch, Ordering::Release);
            Ok(viewing::participants(&members))
        });
        if result.is_err() {
            self.active.store(false, Ordering::Release);
        }
        result
    }
}
static ROOMS: OnceLock<Mutex<HashMap<ChannelId, Weak<Room>>>> = OnceLock::new();
/// Callers must authorize channel visibility and fence delivery against revocation.
pub(crate) async fn roster(channels: Vec<ChannelId>) -> Vec<thiscord_shared::voice::ChannelRoster> {
    let rooms: Vec<_> = {
        let registry = ROOMS.get_or_init(Default::default).lock().await;
        channels
            .into_iter()
            .map(|id| (id, registry.get(&id).and_then(Weak::upgrade)))
            .collect()
    };
    let mut result = Vec::with_capacity(rooms.len());
    for (channel_id, room) in rooms {
        let mut members = Vec::new();
        if let Some(room) = room {
            for member in room.members.read().await.values() {
                let epoch = member.generation.load(Ordering::Acquire);
                if member.active.load(Ordering::Acquire)
                    && member.access.packet(epoch, epoch).is_some()
                {
                    members.push(member.info.clone());
                }
            }
        }
        members.sort_by_key(|m| m.account_id);
        result.push(thiscord_shared::voice::ChannelRoster {
            channel_id,
            members,
        });
    }
    result.sort_by_key(|c| c.channel_id);
    result
}
async fn room(guild: GuildId, id: ChannelId) -> Arc<Room> {
    let mut rooms = ROOMS.get_or_init(Default::default).lock().await;
    rooms.retain(|_, r| r.strong_count() > 0);
    if let Some(room) = rooms.get(&id).and_then(Weak::upgrade) {
        return room;
    }
    let room = Arc::new(Room {
        notification_target: Some((guild, id)),
        ..Default::default()
    });
    rooms.insert(id, Arc::downgrade(&room));
    room
}
pub fn router(origins: Vec<HeaderValue>) -> Router<Option<DbPool>> {
    router_with_limit(origins, 64)
}
pub(crate) async fn diagnostics()
-> HashMap<ChannelId, Vec<thiscord_shared::admin::ParticipantDiagnostics>> {
    diagnostics::snapshot().await
}
pub(crate) fn router_with_limit(
    origins: Vec<HeaderValue>,
    connections: usize,
) -> Router<Option<DbPool>> {
    Router::new()
        .route(VOICE_PATH, get(upgrade))
        .layer(Extension(Arc::new(origins)))
        .layer(Extension(Arc::new(Semaphore::new(connections))))
}
async fn upgrade(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(origins): Extension<Arc<Vec<HeaderValue>>>,
    Extension(limit): Extension<Arc<Semaphore>>,
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
    let Ok(permit) = limit.try_acquire_owned() else {
        return Failure::Limited.response(id);
    };
    ws.max_message_size(128 * 1024)
        .max_frame_size(128 * 1024)
        .on_upgrade(move |socket| async move {
            let mut socket = logging::Socket::new(socket, id);
            socket.permit = Some(permit);
            serve(socket, pool, id).await
        })
        .into_response()
}
async fn send(socket: &mut logging::Socket, event: ServerEvent) -> Result<(), ()> {
    let text = serde_json::to_string(&ServerFrame {
        version: VOICE_VERSION,
        event,
    })
    .map_err(|_| {
        socket.end("signaling_serialize_failed");
    })?;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(text.into())),
    )
    .await;
    if result.is_err() && socket.recoverable {
        socket.end("signaling_send_timeout");
        socket.detach();
        return Ok(());
    }
    result.map_err(|_| {
        socket.end("signaling_send_timeout");
        tracing::warn!(request_id = %socket.id, stage = socket.stage, "voice signaling send timed out");
    })?
    .map_err(|_| ())
}
fn parse(message: Message) -> Option<ClientEvent> {
    let Message::Text(text) = message else {
        return None;
    };
    let frame: ClientFrame = serde_json::from_str(&text).ok()?;
    (frame.version == VOICE_VERSION).then_some(frame.event)
}
async fn fail(socket: &mut logging::Socket, id: RequestId, failure: Failure) {
    let reason = match &failure {
        Failure::Unauthorized => "unauthorized",
        Failure::Forbidden => "forbidden",
        Failure::NotFound => "not_found",
        Failure::Invalid(_) => "invalid_request",
        Failure::Conflict => "conflict",
        Failure::Limited => "rate_limited",
        Failure::Unavailable => "unavailable",
        Failure::Configuration(_) => "configuration",
        Failure::Database(_) => "database",
    };
    socket.end(reason);
    let response = failure.response(id);
    if let Ok(body) = axum::body::to_bytes(response.into_body(), 8192).await
        && let Ok(error) = serde_json::from_slice(&body)
    {
        let _ = send(socket, ServerEvent::Error { error }).await;
    }
}
async fn access_failed(socket: &mut logging::Socket, id: RequestId, failure: Failure) {
    match failure {
        Failure::Unauthorized | Failure::Forbidden => {
            socket.end("access_revoked");
            tracing::info!(request_id = %id, "voice session or channel access revoked");
            let _ = send(socket, ServerEvent::Revoked {}).await;
        }
        error => {
            tracing::warn!(request_id = %id, "voice authorization check failed");
            fail(socket, id, error).await;
        }
    }
}
struct Ingress {
    epoch: u64,
    kind: MediaKind,
    packet: rtp::Packet,
    arrived: Instant,
}
struct Handler {
    request_id: RequestId,
    metrics: Arc<[AtomicU64; 5]>,
    gathered: Arc<Notify>,
    packets: mpsc::Sender<Ingress>,
    media_packets: mpsc::Sender<Ingress>,
    active: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    track_seen: [AtomicBool; 4],
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        tracing::info!(request_id = %self.request_id, state = ?s, "voice peer state changed");
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
        let Some(codec) = track.codec(ssrc).await else {
            return;
        };
        let Some(kind) = MediaKind::from_publisher_ssrc(ssrc) else {
            self.active.store(false, Ordering::Release);
            return;
        };
        let mime = if kind.is_video() {
            "video/H264"
        } else {
            "audio/opus"
        };
        if !codec.mime_type.eq_ignore_ascii_case(mime)
            || codec.clock_rate != kind.clock_rate()
            || self.track_seen[kind as usize].swap(true, Ordering::AcqRel)
        {
            self.active.store(false, Ordering::Release);
            return;
        }
        let tx = if kind == MediaKind::Microphone {
            self.packets.clone()
        } else {
            self.media_packets.clone()
        };
        let metrics = self.metrics.clone();
        let active = self.active.clone();
        let generation = self.generation.clone();
        tokio::spawn(async move {
            let mut since = Instant::now();
            let mut count = 0;
            let mut bytes = 0;
            while let Some(event) = track.poll().await {
                if !active.load(Ordering::Acquire) {
                    break;
                }
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    if since.elapsed() > Duration::from_secs(1) {
                        since = Instant::now();
                        count = 0;
                        bytes = 0;
                    }
                    count += 1;
                    bytes += packet.payload.len();
                    if count
                        > (if kind.is_video() {
                            thiscord_shared::screen::MAX_PACKETS_PER_SECOND
                        } else {
                            100
                        })
                        || packet.payload.len() > 1500
                        || bytes
                            > if kind.is_video() {
                                thiscord_shared::screen::MAX_BYTES_PER_SECOND
                            } else {
                                150_000
                            }
                    {
                        active.store(false, Ordering::Release);
                        break;
                    }
                    if kind.is_video() {
                        metrics[0].fetch_add(1, Ordering::Relaxed);
                    }
                    if tx
                        .try_send(Ingress {
                            epoch: generation.load(Ordering::Acquire),
                            kind,
                            packet,
                            arrived: Instant::now(),
                        })
                        .is_err()
                        && kind.is_video()
                    {
                        metrics[1].fetch_add(1, Ordering::Relaxed);
                    }
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
    let (kind, _) = MediaKind::from_track(slot).expect("negotiated track index");
    let video = kind.is_video();
    Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("speaker-{slot}"),
        format!("speaker-{slot}"),
        format!("speaker-{slot}"),
        if video {
            RtpCodecKind::Video
        } else {
            RtpCodecKind::Audio
        },
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(media_ssrc(slot)),
                ..Default::default()
            },
            codec: RTCRtpCodec {
                mime_type: if video { "video/H264" } else { "audio/opus" }.into(),
                clock_rate: if video { 90_000 } else { 48_000 },
                channels: if video { 0 } else { 2 },
                sdp_fmtp_line: if video {
                    thiscord_shared::screen::H264_FMTP
                } else {
                    "minptime=10;useinbandfec=1"
                }
                .into(),
                ..Default::default()
            },
            ..Default::default()
        }],
    )))
}
fn media_ssrc(index: usize) -> u32 {
    let (kind, slot) = MediaKind::from_track(index).expect("negotiated track index");
    kind.relay_ssrc(slot)
}
fn may_publish(info: &Participant, kind: MediaKind) -> bool {
    info.can_speak
        && !info.deafened
        && match kind {
            MediaKind::Microphone => !info.muted,
            MediaKind::ScreenVideo => info.sharing_screen,
            MediaKind::Camera => info.camera_active,
            MediaKind::SystemAudio => info.sharing_screen && info.sharing_audio,
        }
}
fn may_relay(member: &Member, kind: MediaKind, arrived: Instant) -> bool {
    may_publish(&member.info, kind)
        && (kind == MediaKind::Microphone
            || arrived
                >= if kind == MediaKind::Camera {
                    member.camera_started
                } else {
                    member.screen_started
                })
}
async fn update_voice_state(
    room: &Room,
    slot: usize,
    muted: bool,
    deafened: bool,
    owner: &AtomicBool,
) {
    if let Some(member) = room
        .members
        .write()
        .await
        .get_mut(&slot)
        .filter(|_| owner.load(Ordering::Acquire))
    {
        member.info.muted = muted;
        member.info.deafened = deafened;
        if deafened {
            member.views = [None; ROOM_CAPACITY * 2];
            member.info.camera_active = false;
            member.info.sharing_screen = false;
            member.info.sharing_audio = false;
        }
    }
}
async fn update_screen_state(
    room: &Room,
    slot: usize,
    active: bool,
    audio: bool,
    owner: &AtomicBool,
) -> Result<(), ()> {
    let mut members = room.members.write().await;
    if !owner.load(Ordering::Acquire) {
        return Err(());
    }
    let member = members.get_mut(&slot).ok_or(())?;
    if active && (!member.info.can_speak || member.info.deafened || !member.subscriptions_enabled) {
        return Err(());
    }
    if active && !member.info.sharing_screen {
        member.screen_started = Instant::now();
        member.info.screen_epoch = room
            .screen_epoch
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
    }
    member.info.sharing_screen = active;
    member.info.sharing_audio = active && audio;
    Ok(())
}
async fn handle_signal(
    socket: &mut logging::Socket,
    room: &Room,
    slot: usize,
    event: Option<ClientEvent>,
    owner: &AtomicBool,
) -> Result<(), &'static str> {
    match event {
        Some(ClientEvent::Ping {}) => send(socket, ServerEvent::Pong {})
            .await
            .map_err(|_| "pong_send_failed"),
        Some(ClientEvent::State { muted, deafened }) => {
            update_voice_state(room, slot, muted, deafened, owner).await;
            Ok(())
        }
        Some(ClientEvent::Camera { active }) => {
            let mut members = room.members.write().await;
            let member = members.get_mut(&slot).ok_or("camera_missing_member")?;
            if !owner.load(Ordering::Acquire)
                || (active
                    && (!member.info.can_speak
                        || member.info.deafened
                        || !member.subscriptions_enabled))
            {
                return Err("camera_state_rejected");
            }
            if active && !member.info.camera_active {
                member.camera_started = Instant::now();
                member.info.camera_epoch = room
                    .screen_epoch
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1);
            }
            member.info.camera_active = active;
            Ok(())
        }
        Some(ClientEvent::Screen { active, audio }) => {
            update_screen_state(room, slot, active, audio, owner)
                .await
                .map_err(|_| "screen_state_rejected")
        }
        Some(ClientEvent::ScreenViews { views }) => {
            if viewing::update(room, slot, views, owner).await {
                Ok(())
            } else {
                Err("screen_views_rejected")
            }
        }
        Some(ClientEvent::ScreenKeyframe {
            kind,
            slot: publisher,
            epoch,
        }) => {
            request_keyframe(room, slot, publisher, kind, epoch, owner).await;
            Ok(())
        }
        Some(ClientEvent::Leave {}) => Err("client_leave"),
        None => Err("invalid_frame"),
        _ => Err("unexpected_event"),
    }
}
async fn request_keyframe(
    room: &Room,
    viewer: usize,
    publisher: usize,
    kind: MediaKind,
    epoch: u32,
    owner: &AtomicBool,
) {
    let mut members = room.members.write().await;
    if !kind.is_video() || !owner.load(Ordering::Acquire) {
        return;
    }
    let Some(viewer_epoch) = members
        .get(&viewer)
        .map(|m| m.generation.load(Ordering::Acquire))
    else {
        return;
    };
    let Some(_permit) = members[&viewer].access.packet(viewer_epoch, viewer_epoch) else {
        return;
    };
    if viewer == publisher
        || !members
            .get(&viewer)
            .is_some_and(|m| m.active.load(Ordering::Acquire) && !m.info.deafened)
    {
        return;
    }
    if !members.get(&publisher).is_some_and(|source| {
        members
            .get(&viewer)
            .is_some_and(|receiver| viewing::receives(receiver, source, kind, Instant::now()))
    }) {
        return;
    }
    let Some(source) = members.get_mut(&publisher) else {
        return;
    };
    let source_epoch = source.generation.load(Ordering::Acquire);
    let Some(_source_permit) = source.access.packet(source_epoch, source_epoch) else {
        return;
    };
    if !source.feedback_enabled
        || !source.active.load(Ordering::Acquire)
        || !may_publish(&source.info, kind)
        || source.info.video_epoch(kind) != epoch
    {
        return;
    }
    if source.last_keyframe[kind.view_index(0) / ROOM_CAPACITY]
        .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
    {
        return;
    }
    source.last_keyframe[kind.view_index(0) / ROOM_CAPACITY] = Some(Instant::now());
    let _ = source.keyframes.try_send((kind, epoch));
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
async fn serve(mut socket: logging::Socket, pool: DbPool, id: RequestId) {
    let Ok(Some(Ok(first))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        socket.end("join_timeout");
        return;
    };
    let first = parse(first);
    if let Some(ClientEvent::Resume {
        token,
        guild_id,
        channel_id,
        resume_token,
        sequence,
    }) = first
    {
        resumption::attach(
            socket,
            &pool,
            token,
            guild_id,
            channel_id,
            resume_token,
            sequence,
        )
        .await;
        return;
    }
    let Some(ClientEvent::Join {
        token,
        guild_id,
        channel_id,
        attempt,
    }) = first
    else {
        fail(
            &mut socket,
            id,
            Failure::Invalid("Join voice with version 1 first"),
        )
        .await;
        return;
    };
    socket.stage = "authorize";
    let connection_access = match access::identify(&pool, &token, Some(guild_id)).await {
        Ok((_, access)) => access,
        Err(error) => {
            fail(&mut socket, id, error).await;
            return;
        }
    };
    let mut changed = connection_access.subscribe();
    let authorized = connection_access
        .authorize(&mut changed, || {
            let p = pool.clone();
            let t = token.clone();
            async move {
                tokio::task::spawn_blocking(move || store::authorize(&p, &t, guild_id, channel_id))
                    .await
                    .unwrap_or(Err(Failure::Unavailable))
            }
        })
        .await;
    let ((mut info, voice_revision), epoch) = match authorized {
        Ok(info) => info,
        Err(error) => {
            fail(&mut socket, id, error).await;
            return;
        }
    };
    // Rate-limit once per join, not once per optimistic authorization retry.
    let p = pool.clone();
    let account = info.account_id;
    match tokio::task::spawn_blocking(move || {
        crate::auth::store::rate_limit(
            &p,
            &format!("voice:{account}"),
            Some(&format!("voice:{account}")),
            true,
        )
    })
    .await
    .unwrap_or(Err(Failure::Unavailable))
    {
        Ok(()) => {}
        Err(error) => {
            fail(&mut socket, id, error).await;
            return;
        }
    }
    let generation = Arc::new(AtomicU64::new(epoch));
    let ice_servers = match ice::servers(info.account_id) {
        Ok(servers) => servers,
        Err(_) => {
            fail(&mut socket, id, Failure::Unavailable).await;
            return;
        }
    };
    let room = room(guild_id, channel_id).await;
    let active = Arc::new(AtomicBool::new(true));
    let (out_tx, out_rx) = egress::channel(MediaKind::Microphone);
    let (video_tx, video_rx) = egress::channel(MediaKind::ScreenVideo);
    let (audio_tx, audio_rx) = egress::channel(MediaKind::SystemAudio);
    let (camera_tx, camera_rx) = egress::channel(MediaKind::Camera);
    let metrics: Arc<[AtomicU64; 5]> = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
    let (keyframe_tx, mut keyframe_rx) = mpsc::channel(2);
    let source_id = uuid::Uuid::new_v4();
    let network = Arc::new(RwLock::new(Default::default()));
    info.screen_epoch = room
        .screen_epoch
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    let (replaced_tx, mut replaced) = watch::channel(false);
    let (resume_handle, mut resumes) = resumption::channel(guild_id);
    let resume_token = resume_handle.secret.clone();
    let slot = match membership::admit(
        &room,
        Member {
            resumption: Some(resume_handle),
            request_id: id,
            call_id: attempt.map(|a| a.call_id),
            replaced: replaced_tx,
            network: network.clone(),
            info: info.clone(),
            source_id,
            screen_started: Instant::now(),
            camera_started: Instant::now(),
            outgoing: [out_tx, video_tx, audio_tx, camera_tx],
            keyframes: keyframe_tx,
            last_keyframe: [None; 2],
            feedback_enabled: false,
            subscriptions_enabled: false,
            views: [None; ROOM_CAPACITY * 2],
            generation: generation.clone(),
            access: connection_access.clone(),
            metrics: metrics.clone(),
            active: active.clone(),
        },
        attempt.is_some_and(|a| a.reconnect),
    )
    .await
    {
        Ok(slot) => slot,
        Err(membership::Rejected::Replaced) => {
            replaced_close(&mut socket).await;
            return;
        }
        Err(membership::Rejected::Full) => {
            fail(&mut socket, id, Failure::Limited).await;
            return;
        }
        Err(membership::Rejected::StaleGrant) => {
            fail(&mut socket, id, Failure::Unavailable).await;
            return;
        }
    };
    let gathered = Arc::new(Notify::new());
    let (in_tx, mut in_rx) = mpsc::channel::<Ingress>(8);
    let (media_in_tx, mut media_in_rx) = mpsc::channel::<Ingress>(4096);
    let mut engine = MediaEngine::default();
    engine
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
    let _ = engine.register_default_codecs();
    let registry = register_default_interceptors(Registry::new(), &mut engine)
        .expect("default RTP interceptors");
    socket.stage = "peer_build";
    let built = PeerConnectionBuilder::new()
        .with_configuration(ice::configuration(&ice_servers))
        .with_interceptor_registry(registry)
        .with_media_engine(engine)
        .with_handler(Arc::new(Handler {
            request_id: id,
            metrics: metrics.clone(),
            gathered: gathered.clone(),
            packets: in_tx,
            media_packets: media_in_tx,
            active: active.clone(),
            generation: generation.clone(),
            track_seen: std::array::from_fn(|_| AtomicBool::new(false)),
        }))
        .with_udp_addrs(vec![local_bind().unwrap_or_else(|_| "127.0.0.1:0".into())])
        .build()
        .await;
    let Ok(pc) = built else {
        active.store(false, Ordering::Release);
        release_member(&room, slot, source_id).await;
        if *replaced.borrow() {
            replaced_close(&mut socket).await;
        } else {
            fail(&mut socket, id, Failure::Unavailable).await;
        }
        return;
    };
    let pc: Arc<dyn PeerConnection> = Arc::new(pc);
    let stats_peer = pc.clone();
    let mut stats_task = diagnostics::Sampler(tokio::spawn(async move {
        loop {
            if let Ok(report) = tokio::time::timeout(
                Duration::from_millis(100),
                stats_peer.get_stats(Instant::now(), rtc::statistics::StatsSelector::None),
            )
            .await
            {
                *network.write().await = diagnostics::network(&report);
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }));
    socket.stage = "negotiate";
    let mut tracks = Vec::new();
    let mut screen_feedback = false;
    let mut remote_audio = false;
    let negotiated = async {
        tokio::time::timeout(Duration::from_secs(15), async {
            socket.stage = "add_tracks";
            for slot in 0..MediaKind::TRACK_COUNT {
                let track = local_track(slot);
                pc.add_track(track.clone() as Arc<dyn TrackLocal>)
                    .await
                    .map_err(|_| ())?;
                tracks.push(track);
            }
            socket.stage = "create_offer";
            let offer = pc.create_offer(None).await.map_err(|_| ())?;
            socket.stage = "local_description";
            pc.set_local_description(offer).await.map_err(|_| ())?;
            socket.stage = "ice_gathering";
            gathered.notified().await;
            let sdp =
                serde_json::to_string(&pc.local_description().await.ok_or(())?).map_err(|_| ())?;
            socket.stage = "offer_send";
            send(
                &mut socket,
                ServerEvent::Offer {
                    remote_audio: true,
                    sdp,
                    slot,
                    can_speak: info.can_speak,
                    screen_video: true,
                    camera_video: true,
                    screen_feedback: true,
                    screen_subscriptions: true,
                    ice_servers,
                    resume_token: Some(resume_token),
                },
            )
            .await?;
            socket.stage = "answer";
            let Some(Ok(answer)) = socket.recv().await else {
                return Err(());
            };
            let Some(ClientEvent::Answer {
                sdp,
                screen_feedback: enabled,
                screen_subscriptions,
                remote_audio: audio_enabled,
            }) = parse(answer)
            else {
                return Err(());
            };
            screen_feedback = enabled;
            remote_audio = audio_enabled;
            if let Some(member) = room
                .members
                .write()
                .await
                .get_mut(&slot)
                .filter(|m| m.source_id == source_id)
            {
                member.feedback_enabled = enabled;
                member.subscriptions_enabled = screen_subscriptions;
            }
            let answer: RTCSessionDescription = serde_json::from_str(&sdp).map_err(|_| ())?;
            if answer.sdp_type != RTCSdpType::Answer {
                return Err(());
            }
            socket.stage = "remote_description";
            pc.set_remote_description(answer).await.map_err(|_| ())?;
            Ok(())
        })
        .await
    };
    let negotiated = tokio::select! { biased;
        _ = async { let _ = replaced.wait_for(|value| *value).await; } => None,
        result = negotiated => Some(result),
    };
    if matches!(negotiated, Some(Ok(Ok(())))) {
        socket.recoverable = true;
        socket.stage = "active";
        tracing::info!(request_id = %id, "voice signaling negotiated");
        let relay_room = room.clone();
        let relay_active = active.clone();
        let relay_generation = generation.clone();
        let relay_access = connection_access.clone();
        let relay = tokio::spawn(async move {
            while let Some(Ingress {
                epoch,
                kind,
                mut packet,
                arrived,
            }) = tokio::select! { biased; packet = in_rx.recv() => packet, packet = media_in_rx.recv() => packet }
            {
                if !relay_active.load(Ordering::Acquire) {
                    break;
                }
                let Some(_permit) =
                    relay_access.packet(epoch, relay_generation.load(Ordering::Acquire))
                else {
                    continue;
                };
                let members = relay_room.members.read().await;
                let Some(source) = members.get(&slot) else {
                    break;
                };
                if source.source_id != source_id || !relay_active.load(Ordering::Acquire) {
                    break;
                }
                if !may_relay(source, kind, arrived) {
                    continue;
                }
                packet.header = rtp::header::Header {
                    version: 2,
                    payload_type: kind.payload_type(),
                    ssrc: media_ssrc(kind.track_index(slot)),
                    marker: packet.header.marker,
                    csrc: if kind == MediaKind::Microphone {
                        vec![]
                    } else {
                        vec![source.info.video_epoch(kind)]
                    },
                    sequence_number: packet.header.sequence_number,
                    timestamp: packet.header.timestamp,
                    ..Default::default()
                };
                let mut recovery = Vec::new();
                for (&other, m) in members.iter() {
                    if other != slot {
                        let tx = &m.outgoing[kind as usize];
                        if tx
                            .forward(
                                source,
                                m,
                                Delivery {
                                    epoch,
                                    receiver_epoch: m.generation.load(Ordering::Acquire),
                                    source_access: relay_access.clone(),
                                    slot: kind.track_index(slot),
                                    source: source_id,
                                    source_active: relay_active.clone(),
                                    packet: packet.clone(),
                                    queued_at: arrived,
                                },
                            )
                            .is_err()
                            && kind.is_video()
                        {
                            m.metrics[2].fetch_add(1, Ordering::Relaxed);
                            recovery.push((other, source.info.video_epoch(kind), m.active.clone()));
                        }
                    }
                }
                drop(members);
                drop(_permit);
                for (viewer, epoch, viewer_active) in recovery {
                    request_keyframe(&relay_room, viewer, slot, kind, epoch, &viewer_active).await;
                }
            }
        });
        let tracks = Arc::new(tracks);
        let mut writers = tokio::task::JoinSet::new();
        for (kind, rx) in [
            (MediaKind::Microphone, out_rx),
            (MediaKind::ScreenVideo, video_rx),
            (MediaKind::SystemAudio, audio_rx),
            (MediaKind::Camera, camera_rx),
        ] {
            let writer = egress::Writer {
                access: connection_access.clone(),
                room: room.clone(),
                receiver_slot: slot,
                active: active.clone(),
                generation: generation.clone(),
                metrics: metrics.clone(),
            };
            let tracks = tracks.clone();
            writers.spawn(writer.run(kind, rx, move |slot, packet| {
                let track = tracks[slot].clone();
                async move { track.write_rtp(packet).await.map_err(|_| ()) }
            }));
        }
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let access = VoiceAccess {
            access: connection_access.clone(),
            pool,
            token,
            guild: guild_id,
            channel: channel_id,
            voice_revision,
            generation,
            active: active.clone(),
        };
        let (_audio_registration, mut audio_requests) = if remote_audio {
            let (guard, receiver) = crate::admin::audio::register(id);
            (Some(guard), receiver)
        } else {
            (None, mpsc::channel(1).1)
        };
        let mut audio_call: Option<crate::admin::audio::Call> = None;
        let mut budget = 0;
        let mut audio_reply_budget = 0;
        let mut route_logged = false;
        let mut diagnostic_tick = 0;
        loop {
            tokio::select! {biased;
                _ = async { let _ = replaced.wait_for(|value| *value).await; } => break,
                Some(mut resume) = resumes.recv() => {
                    if socket.recovery_expired() {
                        let _ = send(&mut resume.socket, ServerEvent::ResumeUnavailable {}).await;
                        break;
                    }
                    if !resumption::current(&room, slot, source_id, resume.sequence).await { continue; }
                    if let Err(error) = access.refresh(&room, slot, &mut changed).await {
                        let request_id = resume.socket.id;
                        access_failed(&mut resume.socket, request_id, error).await;
                        break;
                    }
                    socket.end("signaling_reattached");
                    tracing::info!(request_id = %resume.socket.id, media_request_id = %id, sequence = resume.sequence, "voice signaling reattached");
                    socket = resume.socket;
                    socket.stage = "active";
                    socket.recoverable = true;
                    budget = 0;
                    audio_reply_budget = 0;
                    if let Some(call) = audio_call.take() { call.reject(Failure::Unavailable); }
                    if send(&mut socket, ServerEvent::Resumed {}).await.is_err() { break; }
                },
                result=changed.changed()=>{
                    if result.is_err(){socket.end("access_watcher_closed");break;}
                    if let Err(error)=access.refresh(&room,slot,&mut changed).await {
                        access_failed(&mut socket,id,error).await;
                        break;
                    }
                },
                Some((kind, epoch))=keyframe_rx.recv()=>{
                    // Revalidate the publisher and generation after queued feedback.
                    let allowed = connection_access.packet(access.generation.load(Ordering::Acquire), access.generation.load(Ordering::Acquire)).is_some()
                        && room.members.read().await.get(&slot).is_some_and(|m| m.source_id == source_id && m.feedback_enabled && m.active.load(Ordering::Acquire) && m.info.video_active(kind) && m.info.video_epoch(kind) == epoch);
                    if allowed && send(&mut socket, ServerEvent::ScreenKeyframe { kind, epoch }).await.is_err() { break; }
                },
                Some(call)=audio_requests.recv()=>{
                    if socket.detached() { call.reject(Failure::Unavailable); continue; }
                    if audio_call.as_ref().is_some_and(|c| !c.expired()) { call.reject(Failure::Limited); continue; }
                    if call.expired() { continue; }
                    let Some(_admin_delivery) = call.authorized.clone().deliver() else { call.reject(Failure::Forbidden); continue; };
                    let epoch = access.generation.load(Ordering::Acquire);
                    let Some(_target_delivery) = connection_access.packet(epoch, epoch) else { call.reject(Failure::Forbidden); continue; };
                    if !active.load(Ordering::Acquire) { call.reject(Failure::Conflict); continue; }
                    if send(&mut socket, ServerEvent::AudioCommand { request_id: call.id, command: call.command.clone() }).await.is_err() { break; }
                    tracing::info!(request_id = %call.id, connection_id = %id, "admin audio command dispatched");
                    audio_call = Some(call);
                },
                message=socket.recv()=>{
                    let Some(Ok(message))=message else{break;};
                    let event = parse(message);
                    if let Some(ClientEvent::AudioReply { request_id, chunk, done }) = event {
                        audio_reply_budget += 1;
                        if audio_reply_budget > 20 { socket.end("audio_reply_rate_limit"); break; }
                        if let Some(mut call) = audio_call.take() {
                            if call.id != request_id { audio_call = Some(call); continue; }
                            if call.expired() { continue; }
                            if let Err(error) = call.chunk(&chunk) { call.reject(error); }
                            else if done { call.finish(); }
                            else { audio_call = Some(call); }
                        }
                        continue;
                    }
                    budget += 1;
                    // 14 video subscriptions can each request recovery twice/second,
                    // alongside state, subscription, keepalive and publication changes.
                    if budget > 64 { socket.end("signaling_rate_limit"); break; }
                    if let Err(reason)=handle_signal(&mut socket, &room, slot, event, &active).await { socket.end(reason); break; }
                },
                _=tick.tick()=>{
                    if audio_call.as_ref().is_some_and(|c| c.expired()) { audio_call = None; }
                    diagnostic_tick += 1;
                    if screen_feedback && diagnostic_tick % 5 == 0 {
                        let counters = ["sfu_video_ingress_packets", "sfu_video_ingress_dropped", "sfu_video_egress_dropped", "sfu_video_sent_packets", "sfu_video_send_timeouts"].into_iter().enumerate().map(|(i,n)| (n.to_owned(), metrics[i].load(Ordering::Relaxed))).collect();
                        if send(&mut socket, ServerEvent::MediaDiagnostics { counters }).await.is_err() { break; }
                    }
                    budget=0;
                    audio_reply_budget=0;
                    if socket.recovery_expired(){socket.end("signaling_recovery_expired");break;}
                    if socket.silent_for()>Duration::from_secs(20){socket.detach();}
                    if !active.load(Ordering::Acquire){socket.end("media_inactive");break;}
                    let members=match access.refresh(&room,slot,&mut changed).await {
                        Ok(members)=>members,
                        Err(error)=>{access_failed(&mut socket,id,error).await;break;}
                    };
                    for kind in [MediaKind::ScreenVideo, MediaKind::Camera] {
                        if let Some((epoch, bitrate)) = viewing::target(&room, slot, kind).await
                            && send(&mut socket, ServerEvent::ScreenTarget { kind, epoch, bitrate }).await.is_err() { active.store(false, Ordering::Release); break; }
                    }
                    if !route_logged { route_logged=log_selected_route(pc.as_ref(),id).await; }
                if send(&mut socket,ServerEvent::Participants{members}).await.is_err(){break;}
                }
            }
        }
        active.store(false, Ordering::Release);
        relay.abort();
        writers.abort_all();
        let _ = relay.await;
        while writers.join_next().await.is_some() {}
    } else if !*replaced.borrow() {
        socket.end(if matches!(negotiated, Some(Err(_))) {
            "negotiation_timeout"
        } else {
            "negotiation_failed"
        });
    }
    if *replaced.borrow() {
        replaced_close(&mut socket).await;
    }
    active.store(false, Ordering::Release);
    release_member(&room, slot, source_id).await;
    tracing::info!(request_id = %id, "voice membership released");
    stats_task.0.abort();
    let _ = (&mut stats_task.0).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), pc.close()).await;
    if !*replaced.borrow() {
        let _ =
            tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    }
}

async fn release_member(room: &Room, slot: usize, source_id: uuid::Uuid) {
    let mut members = room.members.write().await;
    if members
        .get(&slot)
        .is_some_and(|member| member.source_id == source_id)
        && let Some(member) = members.remove(&slot)
    {
        voice_notification(
            room,
            &members,
            &member.info,
            thiscord_shared::notifications::Kind::VoiceLeave,
        );
    }
}

fn voice_notification(
    room: &Room,
    members: &HashMap<usize, Member>,
    actor: &Participant,
    kind: thiscord_shared::notifications::Kind,
) {
    let Some((guild_id, channel_id)) = room.notification_target else {
        return;
    };
    // Private rooms use the call namespace. Resolve their conversation during delivery.
    let target = thiscord_shared::notifications::Target::Channel {
        guild_id,
        channel_id,
    };
    let verb = if kind == thiscord_shared::notifications::Kind::VoiceJoin {
        "joined voice"
    } else {
        "left voice"
    };
    let mut event = crate::notifications::Candidate::new(
        target,
        kind,
        format!("{} {verb}", actor.display_name()),
        Some(actor.account_id),
    );
    event.recipients = Some(
        members
            .values()
            .filter(|m| m.active.load(Ordering::Acquire))
            .map(|m| m.info.account_id)
            .collect(),
    );
    crate::notifications::publish(event);
}

async fn replaced_close(socket: &mut logging::Socket) {
    socket.end("replaced");
    let _ = send(socket, ServerEvent::Replaced {}).await;
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        socket.send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: REPLACED_CLOSE_CODE,
            reason: "Voice connection replaced".into(),
        }))),
    )
    .await;
}

/// Both endpoints participate in the drain. Epochs belong to each connection,
/// so an unrelated change to one endpoint must not require the other to refresh.
fn delivery_permits(
    source: &Arc<access::Access>,
    source_epoch: u64,
    receiver: &Arc<access::Access>,
    receiver_epoch: u64,
    receiver_authorized: u64,
) -> Option<(access::Packet, access::Packet)> {
    let source = source.packet(source_epoch, source_epoch)?;
    let receiver = receiver.packet(receiver_epoch, receiver_authorized)?;
    Some((source, receiver))
}

/// Poll the bounded transport send only while authorization is current. No lock
/// or mutation permit survives Pending; queue capacity wakes us to revalidate.
/// write_rtp enqueues atomically on its final poll (pinned webrtc 0.21).
async fn authorized_write<F, T, P>(
    room: &Room,
    permit: impl Fn() -> Option<P>,
    allowed: impl Fn(&HashMap<usize, Member>) -> bool,
    send: F,
) -> Result<T, ()>
where
    F: std::future::Future<Output = T>,
{
    use std::{future::Future, task::Poll};
    let mut send = std::pin::pin!(send);
    let mut lock = Box::pin(room.members.read());
    std::future::poll_fn(|cx| {
        let members = match lock.as_mut().poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(members) => members,
        };
        lock = Box::pin(room.members.read());
        let Some(_permit) = permit() else {
            return Poll::Ready(Err(()));
        };
        if !allowed(&members) {
            return Poll::Ready(Err(()));
        }
        send.as_mut().poll(cx).map(Ok)
    })
    .await
}

#[cfg(test)]
mod send_tests {
    use super::*;

    #[test]
    fn queued_media_requires_current_epochs_for_both_endpoints() {
        let identity = || access::Identity {
            account: thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4()),
            session: thiscord_shared::SessionId::from_uuid(uuid::Uuid::new_v4()),
            guild: None,
        };
        let publisher = identity();
        let listener = identity();
        let source = access::register(publisher);
        let receiver = access::register(listener);
        assert!(delivery_permits(&source, 0, &receiver, 0, 0).is_some());
        let mutation = access::pause(access::Scope::Session(publisher.session));
        assert!(delivery_permits(&source, 0, &receiver, 0, 0).is_none());
        drop(mutation);
        let source_epoch = source.snapshot().unwrap();
        assert_ne!(source_epoch, 0);
        assert!(
            delivery_permits(&source, 0, &receiver, 0, 0).is_none(),
            "old publisher queue survived"
        );
        assert!(
            delivery_permits(&source, source_epoch, &receiver, 0, 0).is_some(),
            "independent epochs prevented forwarding"
        );
        let mutation = access::pause(access::Scope::Session(listener.session));
        assert!(delivery_permits(&source, source_epoch, &receiver, 0, 0).is_none());
        drop(mutation);
        let receiver_epoch = receiver.snapshot().unwrap();
        assert!(
            delivery_permits(&source, source_epoch, &receiver, 0, receiver_epoch).is_none(),
            "old receiver queue survived reauthorization"
        );
        assert!(
            delivery_permits(&source, source_epoch, &receiver, receiver_epoch, 0).is_none(),
            "stale receiver authorization admitted new packet"
        );
        assert!(
            delivery_permits(
                &source,
                source_epoch,
                &receiver,
                receiver_epoch,
                receiver_epoch
            )
            .is_some()
        );
    }

    #[tokio::test]
    async fn keyframe_feedback_checks_membership_epoch_deafen_speak_and_coalescing() {
        let room = Room::default();
        let connection_access = access::Access::new();
        let epoch = connection_access.snapshot().unwrap();
        let mut receivers = Vec::new();
        for slot in 0..2 {
            let (keyframes, rx) = mpsc::channel(1);
            receivers.push(rx);
            room.members.write().await.insert(
                slot,
                Member {
                    resumption: None,
                    request_id: RequestId::from_uuid(uuid::Uuid::new_v4()),
                    call_id: None,
                    replaced: watch::channel(false).0,
                    network: Arc::new(RwLock::new(Default::default())),
                    info: Participant {
                        account_id: uuid::Uuid::new_v4().to_string().parse().unwrap(),
                        username: "test".into(),
                        display_name: String::new(),
                        avatar_id: None,
                        slot,
                        muted: false,
                        deafened: false,
                        can_speak: true,
                        sharing_screen: slot == 1,
                        sharing_audio: false,
                        screen_epoch: 7,
                        camera_active: false,
                        camera_epoch: 0,
                        screen_viewers: vec![],
                        camera_viewers: vec![],
                    },
                    source_id: uuid::Uuid::new_v4(),
                    screen_started: Instant::now(),
                    camera_started: Instant::now(),
                    outgoing: [
                        egress::channel(MediaKind::Microphone).0,
                        egress::channel(MediaKind::ScreenVideo).0,
                        egress::channel(MediaKind::SystemAudio).0,
                        egress::channel(MediaKind::Camera).0,
                    ],
                    keyframes,
                    last_keyframe: [None; 2],
                    feedback_enabled: true,
                    subscriptions_enabled: true,
                    views: [None; ROOM_CAPACITY * 2],
                    generation: Arc::new(AtomicU64::new(epoch)),
                    access: connection_access.clone(),
                    active: Arc::new(AtomicBool::new(true)),
                    metrics: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
                },
            );
        }
        let owner = room.members.read().await[&1].info.account_id;
        assert!(
            viewing::update(
                &room,
                0,
                vec![thiscord_shared::screen::Subscription {
                    kind: thiscord_shared::voice::MediaKind::ScreenVideo,
                    slot: 1,
                    owner,
                    epoch: 7,
                    bitrate: 2_500_000,
                }],
                &AtomicBool::new(true)
            )
            .await
        );
        for (viewer, publisher, stream_epoch) in [(7, 1, 7), (0, 7, 7), (1, 1, 7), (0, 1, 6)] {
            request_keyframe(
                &room,
                viewer,
                publisher,
                MediaKind::ScreenVideo,
                stream_epoch,
                &AtomicBool::new(true),
            )
            .await;
            assert!(receivers[1].try_recv().is_err());
        }
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .deafened = true;
        request_keyframe(
            &room,
            0,
            1,
            MediaKind::ScreenVideo,
            7,
            &AtomicBool::new(true),
        )
        .await;
        assert!(receivers[1].try_recv().is_err());
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .deafened = false;
        request_keyframe(
            &room,
            0,
            1,
            MediaKind::ScreenVideo,
            7,
            &AtomicBool::new(true),
        )
        .await;
        assert_eq!(
            receivers[1].try_recv().unwrap(),
            (MediaKind::ScreenVideo, 7)
        );
        request_keyframe(
            &room,
            0,
            1,
            MediaKind::ScreenVideo,
            7,
            &AtomicBool::new(true),
        )
        .await;
        assert!(receivers[1].try_recv().is_err());
        {
            let mut members = room.members.write().await;
            let publisher = members.get_mut(&1).unwrap();
            publisher.last_keyframe = [None; 2];
            publisher.info.can_speak = false;
        }
        request_keyframe(
            &room,
            0,
            1,
            MediaKind::ScreenVideo,
            7,
            &AtomicBool::new(true),
        )
        .await;
        assert!(receivers[1].try_recv().is_err());
    }
    #[tokio::test]
    async fn blocked_transport_releases_room_and_revalidates_before_enqueue() {
        let room = Arc::new(Room::default());
        let allowed = Arc::new(AtomicBool::new(true));
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(1).await.unwrap();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let connection_access = access::Access::new();
        let epoch = connection_access.snapshot().unwrap();
        let task_room = room.clone();
        let task_allowed = allowed.clone();
        let writer = tokio::spawn(async move {
            authorized_write(
                &task_room,
                || connection_access.packet(epoch, epoch),
                |_| task_allowed.load(Ordering::Acquire),
                async {
                    entered.send(()).unwrap();
                    tx.send(2).await
                },
            )
            .await
        });
        waiting.await.unwrap();
        // A waiting room writer and subsequent readers must not wait for the
        // congested transport. Change state before releasing queue capacity.
        let members = tokio::time::timeout(Duration::from_millis(100), room.members.write())
            .await
            .expect("transport held room lock");
        allowed.store(false, Ordering::Release);
        drop(members);
        drop(
            tokio::time::timeout(Duration::from_millis(100), room.members.read())
                .await
                .unwrap(),
        );
        assert_eq!(rx.recv().await, Some(1));
        assert!(writer.await.unwrap().is_err());
        assert_eq!(rx.recv().await, None, "revoked packet was enqueued");
    }
}
