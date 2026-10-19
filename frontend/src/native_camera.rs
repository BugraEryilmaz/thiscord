//! Camera capture stays in the WebView. Only compressed H.264 crosses a loopback
//! WebRTC peer; the authenticated native call transports it to the SFU.
use crate::native_screen::{Binding, ScreenState};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::State;
use thiscord_frontend::audio::connection::Connection;
use thiscord_shared::{screen::ViewAnswer, voice::MediaKind};
use tokio::sync::Notify;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::{
    media_stream::track_remote::{TrackRemote, TrackRemoteEvent},
    peer_connection::*,
};

struct Session {
    id: u32,
    lease: Connection,
    touched: Instant,
    peer: Option<Arc<dyn PeerConnection>>,
    force: Arc<AtomicBool>,
    ready: bool,
    connected: Arc<AtomicBool>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.lease.close();
        if let Some(peer) = self.peer.take() {
            tauri::async_runtime::spawn(async move {
                let _ = peer.close().await;
            });
        }
    }
}
#[derive(Default)]
struct Inner {
    binding: Option<Binding>,
    session: Option<Session>,
}
#[derive(Clone)]
pub struct CameraState {
    inner: Arc<Mutex<Inner>>,
    pub views: ScreenState,
}
impl Default for CameraState {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            views: ScreenState::camera(),
        }
    }
}
impl CameraState {
    pub fn bind(&self, binding: Binding) {
        self.clear();
        self.views.bind(binding.clone());
        if let Ok(mut inner) = self.inner.lock() {
            inner.binding = Some(binding);
        }
    }
    pub fn clear(&self) {
        self.views.clear();
        if let Ok(mut inner) = self.inner.lock() {
            inner.session = None;
            inner.binding = None;
        }
    }
    pub fn stop(&self, session: Option<u32>) {
        if let Ok(mut inner) = self.inner.lock()
            && inner
                .session
                .as_ref()
                .is_some_and(|s| session.is_none_or(|id| id == s.id))
        {
            inner.session = None;
        }
    }
    pub fn active(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner
            .binding
            .as_ref()
            .is_none_or(|b| !b.connection.active())
            || inner.session.as_ref().is_some_and(|s| {
                !s.lease.active()
                    || s.touched.elapsed() > Duration::from_secs(if s.ready { 3 } else { 10 })
            })
        {
            inner.session = None;
        }
        inner
            .session
            .as_ref()
            .is_some_and(|s| s.ready && s.connected.load(Ordering::Acquire))
    }
    pub fn target(&self, bitrate: u32) {
        if let Ok(inner) = self.inner.lock()
            && let Some(b) = &inner.binding
        {
            if b.demand.bitrate() == 0 && bitrate > 0 {
                self.keyframe_locked(&inner);
            }
            b.demand
                .update(bitrate.min(thiscord_shared::camera::MAX_BITRATE));
        }
    }
    fn keyframe_locked(&self, inner: &Inner) {
        if let Some(s) = &inner.session {
            s.force.store(true, Ordering::Release);
        }
    }
    pub fn keyframe(&self) {
        if let Ok(inner) = self.inner.lock()
            && let Some(s) = &inner.session
        {
            s.force.store(true, Ordering::Release);
        }
    }
}
struct Handler {
    gathered: Arc<Notify>,
    binding: Binding,
    lease: Connection,
    force: Arc<AtomicBool>,
    seen: AtomicBool,
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
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.lease.close();
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let Some(ssrc) = track.ssrcs().await.first().copied() else {
            return;
        };
        if self.seen.swap(true, Ordering::AcqRel)
            || track
                .codec(ssrc)
                .await
                .is_none_or(|c| !c.mime_type.eq_ignore_ascii_case("video/H264"))
        {
            self.lease.close();
            return;
        }
        let binding = self.binding.clone();
        let lease = self.lease.clone();
        let force = self.force.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_keyframe = Instant::now() - Duration::from_secs(2);
            let mut timestamp_base = None;
            let mut last_sequence: Option<u16> = None;
            let mut count = 0;
            let mut bytes = 0;
            let mut period = Instant::now();
            loop {
                if !lease.active() || !binding.connection.active() {
                    break;
                }
                tokio::select! {
                    _ = tick.tick() => {
                        if binding.demand.bitrate()>0 && (last_keyframe.elapsed() >= Duration::from_secs(1) || force.swap(false, Ordering::AcqRel)) {
                            last_keyframe = Instant::now();
                            let pli = rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication { sender_ssrc: 1, media_ssrc: ssrc };
                            let _ = tokio::time::timeout(Duration::from_millis(50), track.write_rtcp(vec![Box::new(pli)])).await;
                        }
                    }
                    event = track.poll() => {
                        let Some(event) = event else { lease.close(); break; };
                        if let TrackRemoteEvent::OnRtpPacket(mut packet) = event {
                            if period.elapsed() >= Duration::from_secs(1) { period = Instant::now(); count = 0; bytes = 0; }
                            count += 1; bytes += packet.payload.len();
                            if count > 2000 || bytes > 2_000_000 || packet.payload.len() > 1500 { lease.close(); break; }
                            if binding.demand.bitrate()==0 {last_sequence=None;continue;}
                            // Preserve source gaps: a missing FU-A fragment must remain visible to receivers.
                            let advance=last_sequence.map_or(1,|old|packet.header.sequence_number.wrapping_sub(old));
                            if advance==0 || advance>32768 {continue;}
                            last_sequence=Some(packet.header.sequence_number);
                            let (source, base) = *timestamp_base.get_or_insert_with(|| (packet.header.timestamp, (binding.started.elapsed().as_micros() * 90 / 1000) as u32));
                            packet.header.timestamp = base.wrapping_add(packet.header.timestamp.wrapping_sub(source));
                            packet.header.sequence_number = binding.video_sequence.fetch_add(u32::from(advance), Ordering::Relaxed).wrapping_add(u32::from(advance)) as u16;
                            packet.header.ssrc = MediaKind::Camera.publisher_ssrc();
                            packet.header.payload_type = MediaKind::Camera.payload_type();
                            packet.header.csrc.clear();
                            packet.header.extension = false;
                            packet.header.extensions.clear();
                            if !matches!(tokio::time::timeout(Duration::from_millis(50), binding.video.write_rtp(packet)).await, Ok(Ok(_))) { force.store(true, Ordering::Release); }
                        }
                    }
                }
            }
        });
    }
}
#[tauri::command]
pub fn camera_keepalive(
    state: State<'_, CameraState>,
    session: u32,
) -> thiscord_shared::camera::Status {
    state.active(); // Expire stale leases before allowing a renewal.
    let Ok(mut inner) = state.inner.lock() else {
        return Default::default();
    };
    let bitrate = inner.binding.as_ref().map_or(0, |b| b.demand.bitrate());
    if let Some(s) = inner
        .session
        .as_mut()
        .filter(|s| s.id == session && s.ready && s.lease.active())
    {
        s.touched = Instant::now();
        thiscord_shared::camera::Status {
            alive: true,
            bitrate,
        }
    } else {
        Default::default()
    }
}
#[tauri::command]
pub fn camera_stop(state: State<'_, CameraState>, session: u32) {
    state.stop(Some(session));
}
#[tauri::command]
pub async fn camera_open(
    state: State<'_, CameraState>,
    session: u32,
    sdp: String,
) -> Result<ViewAnswer, String> {
    open(state.inner(), session, sdp).await
}
async fn open(state: &CameraState, session: u32, sdp: String) -> Result<ViewAnswer, String> {
    static NEGOTIATING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let _permit = NEGOTIATING
        .try_acquire()
        .map_err(|_| "Camera is already connecting")?;
    if sdp.len() > 64 * 1024 {
        return Err("Invalid camera offer".into());
    }
    let lease = Connection::default();
    let force = Arc::new(AtomicBool::new(true));
    let connected = Arc::new(AtomicBool::new(false));
    let binding = {
        let mut inner = state.inner.lock().map_err(|_| "Camera unavailable")?;
        let binding = inner
            .binding
            .clone()
            .filter(|b| b.connection.active() && b.can_publish && b.subscriptions)
            .ok_or("Join a call with Speak permission before turning on your camera")?;
        inner.session = Some(Session {
            id: session,
            lease: lease.clone(),
            touched: Instant::now(),
            peer: None,
            ready: false,
            connected: connected.clone(),
            force: force.clone(),
        });
        binding
    };
    let result = async {
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
                    payload_type: 125,
                },
                rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video,
            )
            .map_err(|_| "Cannot configure camera codec")?;
        let registry = register_default_interceptors(Registry::new(), &mut media)
            .map_err(|_| "Cannot configure camera transport")?;
        let gathered = Arc::new(Notify::new());
        let peer: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_runtime(Arc::new(
                    thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
                ))
                .with_media_engine(media)
                .with_interceptor_registry(registry)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    binding,
                    lease: lease.clone(),
                    force,
                    seen: AtomicBool::new(false),
                    connected,
                }))
                .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
                .build()
                .await
                .map_err(|_| "Cannot initialize camera transport")?,
        );
        {
            let mut inner = state.inner.lock().map_err(|_| "Camera unavailable")?;
            let s = inner
                .session
                .as_mut()
                .filter(|s| s.lease.accepts(&lease))
                .ok_or("Camera start cancelled")?;
            s.peer = Some(peer.clone());
        }
        peer.set_remote_description(
            serde_json::from_value(serde_json::json!({"type":"offer","sdp":sdp}))
                .map_err(|_| "Invalid camera offer")?,
        )
        .await
        .map_err(|_| "WebView camera H.264 support is required")?;
        let answer = peer
            .create_answer(None)
            .await
            .map_err(|_| "Cannot negotiate camera")?;
        peer.set_local_description(answer)
            .await
            .map_err(|_| "Cannot initialize camera")?;
        gathered.notified().await;
        let answer = serde_json::to_value(
            peer.local_description()
                .await
                .ok_or("Missing camera answer")?,
        )
        .map_err(|_| "Invalid camera answer")?;
        let sdp = answer["sdp"]
            .as_str()
            .ok_or("Missing camera SDP")?
            .to_owned();
        if !sdp
            .lines()
            .any(|l| l.starts_with("a=rtpmap:") && l.contains("H264/90000"))
        {
            return Err("WebView camera H.264 support is required".into());
        }
        let mut inner = state.inner.lock().map_err(|_| "Camera unavailable")?;
        let s = inner
            .session
            .as_mut()
            .filter(|s| s.lease.accepts(&lease))
            .ok_or("Camera start cancelled")?;
        s.touched = Instant::now();
        s.ready = true;
        Ok(ViewAnswer { sdp })
    };
    let result = tokio::time::timeout(Duration::from_secs(8), result)
        .await
        .unwrap_or_else(|_| Err("Camera connection timed out".into()));
    if result.is_err() {
        state.stop(Some(session));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use webrtc::media_stream::track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP};
    #[test]
    fn camera_commands_are_allowed_only_in_the_local_main_window() {
        // Exercise the same compiled ACL used by the desktop app, without a
        // camera, WebView or permission prompt. Handler registration alone is
        // insufficient when the app has an explicit command allowlist.
        let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let authority = context.runtime_authority_mut();
        for command in ["camera_open", "camera_keepalive", "camera_stop"] {
            assert!(
                authority
                    .resolve_access(command, "main", "main", &tauri::ipc::Origin::Local)
                    .is_some(),
                "{command} must be allowed in the main window"
            );
            for window in ["voice-overlay", "notifications", "unknown"] {
                assert!(
                    authority
                        .resolve_access(command, window, window, &tauri::ipc::Origin::Local)
                        .is_none(),
                    "{command} must not be allowed in {window}"
                );
            }
            assert!(
                authority
                    .resolve_access(
                        command,
                        "main",
                        "main",
                        &tauri::ipc::Origin::Remote {
                            url: "https://example.test".parse().unwrap()
                        }
                    )
                    .is_none(),
                "{command} must not be exposed to remote pages"
            );
        }
    }
    struct Receiver {
        gathered: Arc<Notify>,
        packets: mpsc::Sender<rtc::rtp::Packet>,
    }
    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for Receiver {
        async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
            if s == RTCIceGatheringState::Complete {
                self.gathered.notify_one();
            }
        }
        async fn on_track(&self, track: Arc<dyn TrackRemote>) {
            let tx = self.packets.clone();
            tokio::spawn(async move {
                while let Some(event) = track.poll().await {
                    if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                        let _ = tx.try_send(packet);
                    }
                }
            });
        }
    }
    async fn peer() -> (
        Arc<dyn PeerConnection>,
        Arc<Notify>,
        mpsc::Receiver<rtc::rtp::Packet>,
    ) {
        let gathered = Arc::new(Notify::new());
        let (tx, rx) = mpsc::channel(64);
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
                    payload_type: 125,
                },
                rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video,
            )
            .unwrap();
        let pc = PeerConnectionBuilder::new()
            .with_media_engine(engine)
            .with_runtime(Arc::new(
                thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
            ))
            .with_handler(Arc::new(Receiver {
                gathered: gathered.clone(),
                packets: tx,
            }))
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .build()
            .await
            .unwrap();
        (Arc::new(pc), gathered, rx)
    }
    async fn offer(peer: &Arc<dyn PeerConnection>, gathered: &Notify) -> RTCSessionDescription {
        let offer = peer.create_offer(None).await.unwrap();
        peer.set_local_description(offer).await.unwrap();
        gathered.notified().await;
        peer.local_description().await.unwrap()
    }
    fn binding(video: Arc<TrackLocalStaticRTP>) -> Binding {
        Binding {
            connection: Connection::default(),
            own_slot: 0,
            local_epoch: Default::default(),
            local_visible: Default::default(),
            video,
            audio: thiscord_frontend::audio::transport::track(902),
            can_publish: true,
            subscriptions: true,
            video_sequence: Default::default(),
            audio_sequence: Default::default(),
            audio_timestamp: Default::default(),
            started: Instant::now(),
            force_keyframe: Default::default(),
            demand: Default::default(),
            network_demand: Default::default(),
            metrics: Default::default(),
        }
    }
    #[tokio::test]
    async fn synthetic_camera_crosses_loopback_bridge_and_stops_without_hardware() {
        tokio::time::timeout(Duration::from_secs(25), async {
            let (relay, relay_gathered, _) = peer().await;
            let outgoing = thiscord_frontend::screen::track(MediaKind::Camera.publisher_ssrc());
            relay
                .add_track(outgoing.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            let (viewer, viewer_gathered, mut packets) = peer().await;
            viewer
                .set_remote_description(offer(&relay, &relay_gathered).await)
                .await
                .unwrap();
            let answer = viewer.create_answer(None).await.unwrap();
            viewer.set_local_description(answer).await.unwrap();
            viewer_gathered.notified().await;
            relay
                .set_remote_description(viewer.local_description().await.unwrap())
                .await
                .unwrap();
            let state = CameraState::default();
            state.bind(binding(outgoing));
            let (browser, browser_gathered, _) = peer().await;
            let camera = thiscord_frontend::screen::track(777);
            browser
                .add_track(camera.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            let description =
                serde_json::to_value(offer(&browser, &browser_gathered).await).unwrap();
            let answer = open(&state, 1, description["sdp"].as_str().unwrap().into())
                .await
                .unwrap();
            assert!(!answer.sdp.contains("m=audio"));
            assert!(
                answer
                    .sdp
                    .lines()
                    .filter(|l| l.starts_with("a=candidate:"))
                    .all(|l| l.contains("127.0.0.1"))
            );
            browser
                .set_remote_description(
                    serde_json::from_value(serde_json::json!({"type":"answer","sdp":answer.sdp}))
                        .unwrap(),
                )
                .await
                .unwrap();
            let mut seq = 0u16;
            let packet = loop {
                state.target(1_500_000);
                // Production UI renews this lease; this test owns the native state directly.
                state
                    .inner
                    .lock()
                    .unwrap()
                    .session
                    .as_mut()
                    .unwrap()
                    .touched = Instant::now();
                seq = seq.wrapping_add(1);
                camera
                    .write_rtp(rtc::rtp::Packet {
                        header: rtc::rtp::header::Header {
                            version: 2,
                            ssrc: 777,
                            payload_type: 125,
                            sequence_number: seq,
                            timestamp: u32::from(seq) * 3000,
                            marker: true,
                            ..Default::default()
                        },
                        payload: vec![0x65, 1, 2, 3].into(),
                    })
                    .await
                    .unwrap();
                if let Ok(Some(packet)) =
                    tokio::time::timeout(Duration::from_millis(20), packets.recv()).await
                {
                    break packet;
                }
            };
            assert!(state.active());
            assert_eq!(packet.header.ssrc, MediaKind::Camera.publisher_ssrc());
            assert_eq!(packet.payload.as_ref(), [0x65, 1, 2, 3]);
            state.stop(Some(999));
            assert!(state.active(), "stale stop must not cancel replacement");
            state.stop(Some(1));
            assert!(!state.active());
            state.clear();
            browser.close().await.unwrap();
            relay.close().await.unwrap();
            viewer.close().await.unwrap();
        })
        .await
        .expect("camera bridge should establish and forward without a camera");
    }
    #[tokio::test]
    async fn camera_permission_connection_and_lease_fail_closed() {
        let state = CameraState::default();
        assert!(open(&state, 1, String::new()).await.is_err());
        let mut b = binding(thiscord_frontend::screen::track(903));
        b.can_publish = false;
        state.bind(b.clone());
        assert!(open(&state, 1, String::new()).await.is_err());
        b.can_publish = true;
        b.connection.close();
        state.bind(b);
        assert!(open(&state, 1, String::new()).await.is_err());
        state.bind(binding(thiscord_frontend::screen::track(903)));
        let lease = Connection::default();
        state.inner.lock().unwrap().session = Some(Session {
            id: 2,
            lease: lease.clone(),
            touched: Instant::now() - Duration::from_secs(4),
            peer: None,
            force: Default::default(),
            ready: true,
            connected: Arc::new(AtomicBool::new(true)),
        });
        assert!(!state.active());
        assert!(!lease.active());
    }
}
