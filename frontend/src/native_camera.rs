//! Camera capture stays in the WebView. Only compressed H.264 crosses a local
//! WebRTC peer; the authenticated native call transports it to the SFU.
use crate::native_screen::{Binding, ScreenState};
use std::{
    sync::{
        Arc, Mutex, OnceLock,
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

/// Keep the camera encoder within the baseline format used by the SFU/viewer
/// bridges. rtc falls back to matching H.264 by MIME type when profile bytes do
/// not match exactly; an unfiltered WebKit offer can then select Main or High.
fn camera_offer(
    sdp: &str,
) -> Result<
    (
        String,
        Vec<rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters>,
    ),
    String,
> {
    use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters};
    let mut parsed = rtc::sdp::description::session::SessionDescription::unmarshal(
        &mut std::io::Cursor::new(sdp.as_bytes()),
    )
    .map_err(|_| "Invalid camera offer")?;
    let [media] = parsed.media_descriptions.as_mut_slice() else {
        return Err("Camera offer must contain one video track".into());
    };
    if media.media_name.media != "video" || media.media_name.port.value == 0 {
        return Err("Camera offer must contain one video track".into());
    }
    let offered = media.codecs();
    let codecs: Vec<_> = media
        .media_name
        .formats
        .iter()
        .filter_map(|format| offered.get(&format.parse::<u8>().ok()?))
        .filter(|codec| {
            codec.payload_type <= 127
                && codec.name.eq_ignore_ascii_case("H264")
                && codec.clock_rate == 90_000
                && camera_fmtp(&codec.fmtp)
        })
        .map(|codec| RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: "video/H264".into(),
                clock_rate: 90_000,
                // Register the offered variant verbatim for an exact rtc match.
                sdp_fmtp_line: codec.fmtp.clone(),
                ..Default::default()
            },
            payload_type: codec.payload_type,
        })
        .collect();
    if codecs.is_empty() {
        return Err(
            "This WebView must support baseline H.264 camera video with packetization mode 1"
                .into(),
        );
    }
    let formats: Vec<_> = codecs.iter().map(|c| c.payload_type.to_string()).collect();
    media.attributes.retain(|attribute| {
        if !matches!(attribute.key.as_str(), "rtpmap" | "fmtp" | "rtcp-fb") {
            return true;
        }
        attribute
            .value
            .as_deref()
            .and_then(|value| value.split_whitespace().next())
            .is_some_and(|id| {
                (attribute.key == "rtcp-fb" && id == "*") || formats.iter().any(|f| f == id)
            })
    });
    media.media_name.formats = formats;
    Ok((parsed.marshal(), codecs))
}

fn camera_fmtp(fmtp: &str) -> bool {
    let mut profile = None;
    let mut mode = None;
    for parameter in fmtp.split(';') {
        let Some((key, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "profile-level-id" => &mut profile,
            "packetization-mode" => &mut mode,
            _ => continue,
        };
        if slot.replace(value.trim()).is_some() {
            return false;
        }
    }
    let Some(profile) = profile.filter(|p| p.len() == 6 && p.is_ascii()) else {
        return false;
    };
    let Ok(profile) = u32::from_str_radix(profile, 16) else {
        return false;
    };
    let flags = (profile >> 8) as u8;
    let baseline = match profile >> 16 {
        66 => true,
        77 => flags & 0x8f == 0x80,
        88 => flags & 0xcf == 0xc0,
        _ => false,
    };
    mode == Some("1") && baseline && profile & 0xff != 0
}

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
    capture: Option<(u32, Instant)>,
    stopping: bool,
}
type ReleaseCapture = Box<dyn Fn(CameraState) + Send + Sync>;
#[derive(Clone)]
pub struct CameraState {
    inner: Arc<Mutex<Inner>>,
    release: Arc<OnceLock<ReleaseCapture>>,
    released: Arc<Notify>,
    pub views: ScreenState,
}
impl Default for CameraState {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            release: Default::default(),
            released: Default::default(),
            views: ScreenState::camera(),
        }
    }
}
impl CameraState {
    // Reserve ownership before getUserMedia, including the permission prompt.
    async fn prepare(&self, session: u32) -> Result<(), String> {
        let wait = async {
            loop {
                let notified = self.released.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let mut inner = self.inner.lock().map_err(|_| "Camera unavailable")?;
                    if !inner.stopping {
                        if inner.binding.as_ref().is_none_or(|b| {
                            !b.connection.active() || !b.can_publish || !b.subscriptions
                        }) {
                            return Err(
                                "Join a call with Speak permission before turning on your camera"
                                    .into(),
                            );
                        }
                        if inner.capture.is_some() {
                            return Err("Camera is already starting or sharing".into());
                        }
                        inner.capture = Some((session, Instant::now()));
                        return Ok(());
                    }
                }
                notified.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(3), wait)
            .await
            .unwrap_or_else(|_| Err("Camera is still stopping. Try again.".into()))
    }
    fn release_capture(&self) {
        if let Some(release) = self.release.get() {
            release(self.clone());
        }
    }
    // Called on the WebView thread, not when the stop is queued. A replacement
    // may have acquired the camera in between. Block new starts until WebKit's
    // asynchronous stop completes, so it cannot stop a replacement either.
    #[cfg(any(target_os = "macos", test))]
    fn begin_release(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.capture.is_some() || inner.stopping {
            return false;
        }
        inner.stopping = true;
        true
    }
    #[cfg(any(target_os = "macos", test))]
    fn finish_release(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.stopping = false;
        }
        self.released.notify_waiters();
    }
    #[cfg(target_os = "macos")]
    pub fn attach(&self, window: tauri::WebviewWindow) {
        let _ = self.release.set(Box::new(move |state| {
            let _ = window.with_webview(move |webview| {
                use objc2::{runtime::NSObjectProtocol, sel};
                use objc2_web_kit::{WKMediaCaptureState, WKWebView};
                // SAFETY: Tauri supplies the live WKWebView on its main thread.
                let webview = unsafe { &*webview.inner().cast::<WKWebView>() };
                // This API is available from macOS 12. Older WebKit versions
                // still use the normal MediaStreamTrack.stop cleanup.
                if !webview.respondsToSelector(sel!(setCameraCaptureState:completionHandler:))
                    || !state.begin_release()
                {
                    return;
                }
                let done = block2::RcBlock::new(move || state.finish_release());
                // SAFETY: selector availability was checked; WebKit copies the
                // completion block and calls it after physical capture stops.
                unsafe {
                    webview.setCameraCaptureState_completionHandler(
                        WKMediaCaptureState::None,
                        Some(&done),
                    );
                }
            });
        }));
    }
    pub fn bind(&self, binding: Binding) {
        self.clear();
        self.views.bind(binding.clone());
        if let Ok(mut inner) = self.inner.lock() {
            inner.binding = Some(binding);
        }
    }
    pub fn publication(&self, allowed: bool, boundary: bool) {
        self.views.publication(allowed, boundary);
        let release = if let Ok(mut inner) = self.inner.lock() {
            if let Some(binding) = &mut inner.binding {
                binding.can_publish = allowed;
            }
            if !allowed || boundary {
                inner.session = None; // Drop closes the lease used by live forwarding.
                inner.capture.take().is_some()
            } else {
                false
            }
        } else {
            false
        };
        if release {
            self.release_capture();
        }
    }
    pub fn clear(&self) {
        self.views.clear();
        let release = if let Ok(mut inner) = self.inner.lock() {
            inner.session = None;
            inner.binding = None;
            inner.capture.take().is_some()
        } else {
            false
        };
        if release {
            self.release_capture();
        }
    }
    pub fn stop(&self, session: Option<u32>) {
        let release = if let Ok(mut inner) = self.inner.lock() {
            match inner.capture {
                Some((id, _)) if session.is_none_or(|requested| requested == id) => {
                    inner.session = None;
                    inner.capture = None;
                    true
                }
                // getUserMedia can resolve after its reservation was cancelled.
                // Its explicit cleanup still needs physical capture shutdown.
                None => session.is_some(),
                _ => false,
            }
        } else {
            false
        };
        if release {
            self.release_capture();
        }
    }
    pub fn active(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner
            .binding
            .as_ref()
            .is_none_or(|b| !b.connection.active() || !b.can_publish)
            || inner.session.as_ref().is_some_and(|s| {
                !s.lease.active()
                    || s.touched.elapsed() > Duration::from_secs(if s.ready { 3 } else { 10 })
            })
            || (inner.session.is_none()
                && inner
                    .capture
                    .as_ref()
                    .is_some_and(|(_, started)| started.elapsed() > Duration::from_secs(60)))
        {
            inner.session = None;
            let release = inner.capture.take().is_some();
            drop(inner);
            if release {
                self.release_capture();
            }
            return false;
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
#[tauri::command]
pub async fn camera_prepare(state: State<'_, CameraState>, session: u32) -> Result<(), String> {
    state.prepare(session).await
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
    let (sdp, codecs) = camera_offer(&sdp)?;
    let lease = Connection::default();
    let force = Arc::new(AtomicBool::new(true));
    let connected = Arc::new(AtomicBool::new(false));
    let binding = {
        let mut inner = state.inner.lock().map_err(|_| "Camera unavailable")?;
        if inner.capture.as_ref().is_none_or(|(id, _)| *id != session) {
            return Err("Camera start cancelled".into());
        }
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
        for codec in codecs {
            media
                .register_codec(codec, rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video)
                .map_err(|_| "Cannot configure camera codec")?;
        }
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
                .with_udp_addrs(thiscord_frontend::screen::local_bridge_udp_addrs())
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
    fn camera_offer_rejects_incompatible_only_codecs_before_bridge_creation() {
        for fmtp in [
            "packetization-mode=1;profile-level-id=64001f",
            "packetization-mode=1;profile-level-id=4d001f",
            "packetization-mode=0;profile-level-id=42e01f",
            "profile-level-id=42e01f",
            "packetization-mode=1;profile-level-id=42e01f;profile-level-id=64001f",
        ] {
            let sdp = format!(
                "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\na=fmtp:102 {fmtp}\r\n"
            );
            assert!(camera_offer(&sdp).is_err(), "must reject {fmtp}");
        }
    }

    #[tokio::test]
    async fn camera_answer_selects_baseline_variant_even_when_high_profile_is_first() {
        use rtc::sdp::{description::common::Attribute, description::session::SessionDescription};
        async fn answer(
            sdp: String,
            codecs: Vec<rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters>,
        ) -> SessionDescription {
            let mut engine = MediaEngine::default();
            for codec in codecs {
                engine
                    .register_codec(codec, rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video)
                    .unwrap();
            }
            let answerer = PeerConnectionBuilder::new()
                .with_media_engine(engine)
                .with_runtime(Arc::new(
                    thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
                ))
                .with_handler(Arc::new(Receiver {
                    gathered: Arc::new(Notify::new()),
                    packets: mpsc::channel(1).0,
                }))
                .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
                .build()
                .await
                .unwrap();
            answerer
                .set_remote_description(
                    serde_json::from_value(serde_json::json!({"type":"offer","sdp":sdp})).unwrap(),
                )
                .await
                .unwrap();
            let answer = serde_json::to_value(answerer.create_answer(None).await.unwrap()).unwrap();
            let answer = SessionDescription::unmarshal(&mut std::io::Cursor::new(
                answer["sdp"].as_str().unwrap().as_bytes(),
            ))
            .unwrap();
            answerer.close().await.unwrap();
            answer
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            let (browser, gathered, _) = peer().await;
            browser
                .add_track(thiscord_frontend::screen::track(777) as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            let original = serde_json::to_value(offer(&browser, &gathered).await).unwrap();
            for profile in ["42c01f", "42001f", "4d801f", "58c01f"] {
                let mut offer = SessionDescription::unmarshal(&mut std::io::Cursor::new(
                    original["sdp"].as_str().unwrap().as_bytes(),
                ))
                .unwrap();
                let media = &mut offer.media_descriptions[0];
                media.media_name.formats = ["102", "103", "104", "105"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                media.attributes.retain(|a| {
                    !matches!(a.key.as_str(), "rtpmap" | "fmtp" | "rtcp-fb")
                });
                for (pt, offered_profile, mode) in [
                    (102, "64001f", 1),
                    (103, "4d001f", 1),
                    (104, "42e01f", 0),
                    (105, profile, 1),
                ] {
                    media.attributes.push(Attribute::new(
                        "rtpmap".into(),
                        Some(format!("{pt} H264/90000")),
                    ));
                    media.attributes.push(Attribute::new(
                        "fmtp".into(),
                        Some(format!("{pt} level-asymmetry-allowed=1;packetization-mode={mode};profile-level-id={offered_profile}")),
                    ));
                }
                let offer = offer.marshal();
                // The previous single 42e0 codec registration accepts the
                // incompatible first High profile through rtc's MIME fallback.
                let (_, previous_codecs) = camera_offer(original["sdp"].as_str().unwrap()).unwrap();
                let previous = answer(offer.clone(), previous_codecs).await;
                assert_eq!(previous.media_descriptions[0].media_name.formats[0], "102");
                assert!(previous.media_descriptions[0].codecs()[&102].fmtp.contains("64001f"));
                let (filtered, codecs) = camera_offer(&offer).unwrap();
                assert_eq!(codecs.len(), 1);
                assert_eq!(codecs[0].payload_type, 105);
                let answer = answer(filtered, codecs).await;
                assert_eq!(answer.media_descriptions[0].media_name.formats, ["105"]);
                let codecs = answer.media_descriptions[0].codecs();
                assert_eq!(codecs.len(), 1);
                assert!(codecs[&105].fmtp.contains(profile));
            }
            browser.close().await.unwrap();
        }).await.expect("camera codec negotiation should complete without camera hardware");
    }
    #[test]
    fn camera_commands_are_allowed_only_in_the_local_main_window() {
        // Exercise the same compiled ACL used by the desktop app, without a
        // camera, WebView or permission prompt. Handler registration alone is
        // insufficient when the app has an explicit command allowlist.
        let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let authority = context.runtime_authority_mut();
        for command in [
            "camera_prepare",
            "camera_open",
            "camera_keepalive",
            "camera_stop",
        ] {
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
        peer_on("127.0.0.1:0").await
    }
    /// Default-route address, as an enumerating WebView would bind it.
    fn interface_ip() -> std::net::IpAddr {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:9").unwrap();
        socket.local_addr().unwrap().ip()
    }
    async fn peer_on(
        bind: &str,
    ) -> (
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
            .with_udp_addrs(vec![bind.to_owned()])
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
            layers: None,
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
    async fn synthetic_camera_crosses_local_bridge_and_stops_without_hardware() {
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
            state.prepare(1).await.unwrap();
            // Windows cannot send from an interface-bound WebView socket to loopback.
            let (browser, browser_gathered, _) = peer_on(&format!("{}:0", interface_ip())).await;
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
            let candidates: Vec<_> = answer
                .sdp
                .lines()
                .filter(|l| l.starts_with("a=candidate:"))
                .collect();
            for ip in ["127.0.0.1".to_owned(), interface_ip().to_string()] {
                assert!(
                    candidates.iter().any(|l| l.contains(&format!(" {ip} "))),
                    "answer must offer {ip}"
                );
            }
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
        assert!(state.prepare(1).await.is_err());
        assert!(open(&state, 1, String::new()).await.is_err());
        let mut b = binding(thiscord_frontend::screen::track(903));
        b.can_publish = false;
        state.bind(b.clone());
        assert!(state.prepare(1).await.is_err());
        assert!(open(&state, 1, String::new()).await.is_err());
        b.can_publish = true;
        b.connection.close();
        state.bind(b);
        assert!(state.prepare(1).await.is_err());
        assert!(open(&state, 1, String::new()).await.is_err());
        state.bind(binding(thiscord_frontend::screen::track(903)));
        state.prepare(2).await.unwrap();
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
        assert!(state.inner.lock().unwrap().capture.is_none());
    }

    #[tokio::test]
    async fn moderation_cancels_capture_and_pending_permission_without_auto_resume() {
        let state = CameraState::default();
        state.bind(binding(thiscord_frontend::screen::track(903)));
        state.prepare(1).await.unwrap();
        let lease = Connection::default();
        state.inner.lock().unwrap().session = Some(Session {
            id: 1,
            lease: lease.clone(),
            touched: Instant::now(),
            peer: None,
            force: Default::default(),
            ready: true,
            connected: Arc::new(AtomicBool::new(true)),
        });
        assert!(state.active());
        state.publication(false, true);
        assert!(
            !lease.active(),
            "live bridge must stop forwarding immediately"
        );
        assert!(!state.active());
        assert!(state.prepare(2).await.is_err());
        state.publication(true, true);
        assert!(!state.active(), "release is not a capture start");
        state.prepare(3).await.unwrap();
        // Both moderation edges were missed, but the revision still changed.
        state.publication(true, true);
        assert!(state.inner.lock().unwrap().capture.is_none());
        assert!(open(&state, 3, String::new()).await.is_err());
        state.prepare(4).await.unwrap();
        state.publication(true, false);
        assert_eq!(
            state.inner.lock().unwrap().capture.unwrap().0,
            4,
            "unchanged roster must not cancel a new explicit start"
        );
    }

    #[tokio::test]
    async fn capture_stop_covers_permission_prompt_and_rejects_late_open() {
        let state = CameraState::default();
        state.bind(binding(thiscord_frontend::screen::track(903)));
        state.prepare(1).await.unwrap();
        state.stop(Some(1));
        assert!(state.inner.lock().unwrap().capture.is_none());
        state.prepare(2).await.unwrap();
        state.stop(Some(1));
        assert!(
            !state.begin_release(),
            "queued old stop must not release replacement"
        );
        assert!(open(&state, 1, String::new()).await.is_err());
        assert_eq!(state.inner.lock().unwrap().capture.unwrap().0, 2);
    }

    #[tokio::test]
    async fn replacement_waits_for_physical_camera_shutdown() {
        let state = CameraState::default();
        state.bind(binding(thiscord_frontend::screen::track(903)));
        state.prepare(1).await.unwrap();
        state.stop(Some(1));
        assert!(state.begin_release());
        let next = state.prepare(2);
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut next)
                .await
                .is_err()
        );
        state.finish_release();
        next.await.unwrap();
        assert_eq!(state.inner.lock().unwrap().capture.unwrap().0, 2);
    }

    #[tokio::test]
    async fn native_teardown_releases_capture_without_webview_keepalive() {
        let state = CameraState::default();
        let releases = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count = releases.clone();
        assert!(
            state
                .release
                .set(Box::new(move |state| {
                    if state.begin_release() {
                        count.fetch_add(1, Ordering::Relaxed);
                        state.finish_release();
                    }
                }))
                .is_ok()
        );
        let b = binding(thiscord_frontend::screen::track(903));
        state.bind(b.clone());
        state.prepare(1).await.unwrap();
        let before = releases.load(Ordering::Relaxed);
        b.connection.close();
        assert!(!state.active());
        assert_eq!(releases.load(Ordering::Relaxed), before + 1);
        state.bind(binding(thiscord_frontend::screen::track(903)));
        state.prepare(2).await.unwrap();
        state.inner.lock().unwrap().capture.as_mut().unwrap().1 =
            Instant::now() - Duration::from_secs(61);
        let before = releases.load(Ordering::Relaxed);
        assert!(!state.active());
        assert_eq!(releases.load(Ordering::Relaxed), before + 1);
        state.prepare(3).await.unwrap();
        let before = releases.load(Ordering::Relaxed);
        state.clear();
        assert_eq!(releases.load(Ordering::Relaxed), before + 1);
        assert!(state.prepare(4).await.is_err());
        // A permission prompt can finish after call teardown. An explicit late
        // track cleanup must still reach WebKit even without a native session.
        let before = releases.load(Ordering::Relaxed);
        state.stop(Some(3));
        assert_eq!(releases.load(Ordering::Relaxed), before + 1);
        state.stop(None);
        assert_eq!(
            releases.load(Ordering::Relaxed),
            before + 1,
            "idle polling must not stop capture repeatedly"
        );
    }
}
