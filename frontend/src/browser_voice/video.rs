//! Browser screen sharing and cameras. The browser captures, encodes (H.264
//! baseline), decodes and renders; WASM only routes tracks between capture,
//! the SFU m-lines and `<video>` elements, sets encoder limits and signals
//! viewing demand and keyframe requests. No pixels pass through Rust.
//!
//! Publishing reuses the own-slot screen, shared-audio and camera m-lines the
//! answer negotiated `sendrecv` with an idle sender, so starting, switching or
//! stopping media is `replaceTrack` without renegotiation.
//! Note: web-sys `MediaStream::clone`/`MediaStreamTrack::clone` call the JS
//! `clone()` (new tracks); handles are copied with `Clone::clone` here.
use super::call::{Call, now};
use js_sys::{Array, Function, Reflect};
use leptos::prelude::*;
use std::{cell::Cell, rc::Rc};
use thiscord_frontend::{
    browser_audio::RemoteSource,
    browser_video::{self as rules, Inbound, Recovery, VideoSupport},
    video_feedback::{DecoderPressure, DecoderSample},
};
use thiscord_shared::{
    screen::{Quality, Subscription, Watch},
    video_feedback::Receiver as Budget,
    voice::{ClientEvent, MediaKind, Participant, ROOM_CAPACITY},
};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    HtmlVideoElement, MediaStream, MediaStreamTrack, MediaStreamTrackState, RtcRtpSender,
    RtcRtpTransceiver,
};

fn json(value: serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::UNDEFINED)
}
fn call_optional(target: &JsValue, method: &str, args: &Array) -> Option<Result<JsValue, JsValue>> {
    let function = Reflect::get(target, &method.into())
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    Some(Reflect::apply(&function, target, args))
}
fn text(value: &JsValue, key: &str) -> Option<String> {
    Reflect::get(value, &key.into()).ok()?.as_string()
}
fn number(value: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(value, &key.into()).ok()?.as_f64()
}
fn error_name(error: &JsValue) -> String {
    text(error, "name").unwrap_or_default()
}
fn stop_stream(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        track.unchecked_into::<MediaStreamTrack>().stop();
    }
}
fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| !d.hidden())
}

/// Own-slot sender index: screen video, shared screen audio, camera.
pub(super) fn sender_index(kind: MediaKind) -> Option<usize> {
    match kind {
        MediaKind::ScreenVideo => Some(0),
        MediaKind::SystemAudio => Some(1),
        MediaKind::Camera => Some(2),
        MediaKind::Microphone => None,
    }
}

/// Baseline H.264 capabilities (`RTCRtpReceiver`/`RTCRtpSender`), constrained
/// baseline first. Empty or `None` means this browser cannot use the SFU's video.
fn h264_capabilities(class: &str) -> Option<Array> {
    let class = Reflect::get(&js_sys::global(), &class.into()).ok()?;
    let capabilities =
        call_optional(&class, "getCapabilities", &Array::of1(&"video".into()))?.ok()?;
    let codecs: Array = Reflect::get(&capabilities, &"codecs".into())
        .ok()?
        .dyn_into()
        .ok()?;
    let mut usable: Vec<(u8, JsValue)> = codecs
        .iter()
        .filter_map(|codec| {
            let mime = text(&codec, "mimeType")?;
            let fmtp = text(&codec, "sdpFmtpLine").unwrap_or_default();
            rules::usable_h264(&mime, &fmtp).then(|| (rules::h264_rank(&fmtp), codec))
        })
        .collect();
    usable.sort_by_key(|(rank, _)| *rank);
    let list = usable
        .into_iter()
        .map(|(_, codec)| codec)
        .collect::<Array>();
    (list.length() > 0).then_some(list)
}
/// What this browser can do with the SFU's H.264 video.
pub(super) fn support() -> VideoSupport {
    VideoSupport {
        decode: h264_capabilities("RTCRtpReceiver").is_some(),
        encode: h264_capabilities("RTCRtpSender").is_some(),
    }
}
/// Limits a video transceiver to baseline H.264. `false` where the browser
/// lacks `setCodecPreferences`; the caller then restricts the answer SDP.
pub(super) fn prefer_h264(transceiver: &RtcRtpTransceiver) -> bool {
    let Some(codecs) = h264_capabilities("RTCRtpReceiver") else {
        return false;
    };
    matches!(
        call_optional(transceiver, "setCodecPreferences", &Array::of1(&codecs)),
        Some(Ok(_))
    )
}

/// `navigator.mediaDevices.getDisplayMedia` exists (not on iOS or Android).
pub fn screen_supported() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().media_devices().ok())
        .and_then(|devices| Reflect::get(&devices, &"getDisplayMedia".into()).ok())
        .is_some_and(|f| f.is_function())
}
/// Chromium browsers (Chrome, Edge) can include tab or system audio.
pub fn screen_audio_supported() -> bool {
    web_sys::window()
        .and_then(|w| Reflect::get(&w.navigator(), &"userAgentData".into()).ok())
        .is_some_and(|value| value.is_object())
}

/// Received video and own-slot senders of one negotiated attempt.
#[derive(Default)]
pub(super) struct Media {
    pub senders: [Option<RtcRtpSender>; 3],
    pub remote: Vec<Remote>,
    pub support: VideoSupport,
    /// Other members' received shared screen audio, by member slot.
    pub shared_audio: Vec<(usize, MediaStreamTrack)>,
    /// Members whose shared audio is currently stopped locally.
    audio_stopped: [bool; ROOM_CAPACITY],
    /// The last `ScreenViews` list sent (empty before the first one).
    sent_views: Vec<thiscord_shared::screen::Subscription>,
    views_at: f64,
    stats_at: f64,
}
impl Media {
    pub(super) fn new(support: VideoSupport) -> Self {
        Self {
            support,
            ..Default::default()
        }
    }
    /// A resumed socket gets the current views on the next tick.
    pub(super) fn resend_views(&mut self) {
        self.sent_views.clear();
        self.views_at = 0.0;
    }
}
pub(super) struct Remote {
    pub kind: MediaKind,
    pub slot: usize,
    pub mid: String,
    pub track: MediaStreamTrack,
}

/// One local capture published on the own-slot senders.
struct Capture {
    stream: MediaStream,
    video: MediaStreamTrack,
    audio: Option<MediaStreamTrack>,
    quality: Quality,
    target: Option<u32>,
    applied: Option<rules::EncoderLimit>,
    applying: Rc<Cell<bool>>,
    ended: Rc<Cell<bool>>,
    _ended: Closure<dyn FnMut()>,
}
impl Capture {
    fn new(stream: MediaStream, quality: Quality) -> Option<Self> {
        let video: MediaStreamTrack = stream.get_video_tracks().get(0).dyn_into().ok()?;
        let audio = stream.get_audio_tracks().get(0).dyn_into().ok();
        let ended = Rc::new(Cell::new(false));
        let flag = ended.clone();
        // The browser's own "Stop sharing" control, an unplugged camera or a
        // revoked permission end the track; the next tick unpublishes it.
        let listener = Closure::<dyn FnMut()>::new(move || flag.set(true));
        let _ = video.add_event_listener_with_callback("ended", listener.as_ref().unchecked_ref());
        Some(Self {
            stream,
            video,
            audio,
            quality,
            target: None,
            applied: None,
            applying: Rc::new(Cell::new(false)),
            ended,
            _ended: listener,
        })
    }
    fn live(&self) -> bool {
        !self.ended.get() && self.video.ready_state() == MediaStreamTrackState::Live
    }
    fn stop(&self) {
        let _ = self
            .video
            .remove_event_listener_with_callback("ended", self._ended.as_ref().unchecked_ref());
        stop_stream(&self.stream);
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Screen and camera publication of one call. Never restarted on rejoin.
#[derive(Default)]
pub(super) struct Publishing {
    screen: Option<Capture>,
    camera: Option<Capture>,
    facing: rules::Facing,
    /// Front/back flipping on phones overrides the saved camera for this call.
    facing_only: bool,
    /// A camera switch is in progress: browsers (notably WebKit) may end the
    /// previous track as soon as the next camera opens.
    switching: bool,
}

/// Publication state for the voice bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub screen: bool,
    pub screen_audio: bool,
    pub camera: bool,
    pub error: String,
}
thread_local! {
    static STATE: ArcRwSignal<State> = ArcRwSignal::new(State::default());
    static VIEWS: std::cell::RefCell<Vec<View>> = const { std::cell::RefCell::new(Vec::new()) };
    static NEXT_VIEW: Cell<u32> = const { Cell::new(1) };
}
/// Reactive publication state.
pub fn state() -> State {
    STATE.with(|state| state.get())
}
fn update_state(f: impl FnOnce(&mut State)) {
    STATE.with(|state| {
        let mut next = state.get_untracked();
        f(&mut next);
        if next != state.get_untracked() {
            state.set(next);
        }
    });
}
pub fn clear_error() {
    update_state(|s| s.error.clear());
}
fn fail(message: &str) {
    update_state(|s| s.error = message.to_owned());
}
fn publishing_state(publishing: &Publishing) {
    update_state(|s| {
        s.screen = publishing.screen.is_some();
        s.screen_audio = publishing
            .screen
            .as_ref()
            .is_some_and(|c| c.audio.is_some());
        s.camera = publishing.camera.is_some();
    });
    crate::cameras::set_call_camera(publishing.camera.is_some());
}

fn media_error(kind: MediaKind, error: &JsValue) -> Option<&'static str> {
    let camera = kind == MediaKind::Camera;
    Some(match error_name(error).as_str() {
        // Cancelling the browser's share picker is not an error.
        "NotAllowedError" | "AbortError" if !camera => return None,
        "NotAllowedError" | "SecurityError" | "PermissionDeniedError" => {
            "Camera access is blocked. Allow the camera for this site in your browser settings."
        }
        "NotFoundError" | "DevicesNotFoundError" | "OverconstrainedError" if camera => {
            "No camera found. Connect a webcam or enable a virtual camera."
        }
        "NotReadableError" | "TrackStartError" | "AbortError" if camera => {
            "The camera is busy or unavailable. Close other camera apps and try again."
        }
        _ if camera => "Cannot start the camera in this browser.",
        _ => "Cannot start screen sharing in this browser.",
    })
}

/// Why this member cannot publish right now, if so.
fn blocked(call: &Call, kind: MediaKind) -> Option<&'static str> {
    let live = call.live.borrow();
    let Some(live) = live.as_ref().filter(|l| l.answered()) else {
        return Some("Join voice first, then try again.");
    };
    let Some(own) = live.own() else {
        return Some("Join voice first, then try again.");
    };
    if !own.can_speak || own.server_muted || own.is_deafened() || super::settings().deafened {
        return Some("You can’t share in this call while deafened or without Speak permission.");
    }
    let index = sender_index(kind)?;
    if live.media.senders[index].is_none() {
        return Some(if live.media.support.encode {
            "Rejoin voice to share. Speak permission was granted after joining."
        } else {
            "This browser can’t send H.264 video, which Thiscord calls use."
        });
    }
    None
}

/// Starts screen sharing. Call synchronously from the click: browsers only
/// open the share picker inside a user gesture.
pub fn start_screen(
    quality: Quality,
    audio: bool,
) -> impl std::future::Future<Output = ()> + 'static {
    clear_error();
    let promise = (|| {
        let devices = web_sys::window()?.navigator().media_devices().ok()?;
        let constraints = json(serde_json::json!({
            "video": rules::display_constraints(quality),
            "audio": audio,
            "systemAudio": if audio { "include" } else { "exclude" },
            // Sharing this tab would loop the call back into itself.
            "selfBrowserSurface": "exclude",
            "surfaceSwitching": "include",
        }));
        call_optional(&devices, "getDisplayMedia", &Array::of1(&constraints))?
            .ok()?
            .dyn_into::<js_sys::Promise>()
            .ok()
    })();
    // Bind the capture to the call it was requested in (see `publish`).
    let origin = super::current();
    async move {
        let Some(promise) = promise else {
            fail("Screen sharing isn't supported in this browser");
            return;
        };
        match JsFuture::from(promise).await {
            Ok(stream) => {
                publish(
                    origin,
                    MediaKind::ScreenVideo,
                    stream.unchecked_into(),
                    quality,
                )
                .await
            }
            Err(error) => {
                if let Some(message) = media_error(MediaKind::ScreenVideo, &error) {
                    fail(message);
                }
            }
        }
    }
}

/// The saved camera (with the missing-device fallback), or the facing mode
/// after a front/back flip.
async fn camera_stream(call: &Call) -> Result<MediaStream, JsValue> {
    let (facing, facing_only) = {
        let publishing = call.publishing.borrow();
        (publishing.facing, publishing.facing_only)
    };
    crate::cameras::open(Some(facing), !facing_only).await
}

pub async fn start_camera() {
    clear_error();
    let Some(call) = super::current() else {
        fail("Join voice first, then try again.");
        return;
    };
    if let Some(message) = blocked(&call, MediaKind::Camera) {
        fail(message);
        return;
    }
    match camera_stream(&call).await {
        Ok(stream) => publish(Some(call), MediaKind::Camera, stream, Quality::default()).await,
        Err(error) => fail(media_error(MediaKind::Camera, &error).unwrap_or_default()),
    }
}

/// Publishes a new capture on the own-slot senders, replacing any previous
/// capture of that kind. The stream is stopped on every failure path, and
/// when the call it was requested in (`origin`) ended during the permission
/// prompt or picker, so a capture never lands in a later call.
async fn publish(origin: Option<Rc<Call>>, kind: MediaKind, stream: MediaStream, quality: Quality) {
    let Some(call) = super::current().filter(|call| origin.is_some_and(|o| Rc::ptr_eq(&o, call)))
    else {
        stop_stream(&stream);
        return;
    };
    if let Some(message) = blocked(&call, kind) {
        stop_stream(&stream);
        fail(message);
        return;
    }
    let Some(capture) = Capture::new(Clone::clone(&stream), quality) else {
        stop_stream(&stream);
        fail(media_error(kind, &JsValue::NULL).unwrap_or_default());
        return;
    };
    let senders = call
        .live
        .borrow()
        .as_ref()
        .map(|live| live.media.senders.clone())
        .unwrap_or_default();
    let audio = if kind == MediaKind::ScreenVideo {
        capture.audio.clone().filter(|_| senders[1].is_some())
    } else {
        None
    };
    let hint = rules::content_hint(kind, quality.fps);
    let _ = Reflect::set(&capture.video, &"contentHint".into(), &hint.into());
    // Signal first: the SFU forwards packets that arrive after the start.
    let restarting = {
        let publishing = call.publishing.borrow();
        match kind {
            MediaKind::Camera => publishing.camera.is_some(),
            _ => publishing.screen.is_some(),
        }
    };
    if !restarting {
        call.send(match kind {
            MediaKind::Camera => ClientEvent::Camera { active: true },
            _ => ClientEvent::Screen {
                active: true,
                audio: audio.is_some(),
                quality: None,
            },
        });
    }
    let video_sender = senders[sender_index(kind).unwrap_or(0)].clone();
    let mut attached = match &video_sender {
        Some(sender) => JsFuture::from(sender.replace_track(Some(&capture.video)))
            .await
            .is_ok(),
        None => false,
    };
    if attached
        && kind == MediaKind::ScreenVideo
        && let Some(sender) = &senders[1]
    {
        attached = JsFuture::from(sender.replace_track(audio.as_ref()))
            .await
            .is_ok();
    }
    // The call may have ended, changed attempt or been deafened meanwhile.
    let current = super::current().is_some_and(|c| c.id == call.id)
        && call
            .live
            .borrow()
            .as_ref()
            .is_some_and(|l| l.media.senders == senders);
    if !attached || !current || blocked(&call, kind).is_some() {
        drop(capture);
        if current {
            stop(&call, kind, !restarting);
        }
        if !attached {
            fail(if kind == MediaKind::Camera {
                "Cannot start the camera in this browser."
            } else {
                "Cannot start screen sharing in this browser."
            });
        }
        return;
    }
    {
        let mut publishing = call.publishing.borrow_mut();
        let slot = match kind {
            MediaKind::Camera => &mut publishing.camera,
            _ => &mut publishing.screen,
        };
        let previous = slot.replace(capture);
        drop(previous);
        publishing_state(&publishing);
    }
    // Apply the preset ceiling now; ScreenTarget then follows viewer demand.
    apply_limits(&call);
}

/// Stops one kind of publication: idle senders, stopped tracks, signaled stop.
fn stop(call: &Call, kind: MediaKind, signal: bool) {
    let capture = {
        let mut publishing = call.publishing.borrow_mut();
        let capture = match kind {
            MediaKind::Camera => publishing.camera.take(),
            _ => publishing.screen.take(),
        };
        publishing_state(&publishing);
        capture
    };
    let senders = call
        .live
        .borrow()
        .as_ref()
        .map(|live| live.media.senders.clone())
        .unwrap_or_default();
    let indexes: &[usize] = if kind == MediaKind::Camera {
        &[2]
    } else {
        &[0, 1]
    };
    for sender in indexes.iter().filter_map(|i| senders[*i].clone()) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(sender.replace_track(None)).await;
        });
    }
    if signal || capture.is_some() {
        call.send(match kind {
            MediaKind::Camera => ClientEvent::Camera { active: false },
            _ => ClientEvent::Screen {
                active: false,
                audio: false,
                quality: None,
            },
        });
    }
    drop(capture);
}
pub fn stop_screen() {
    if let Some(call) = super::current() {
        stop(&call, MediaKind::ScreenVideo, false);
    }
}
pub fn stop_camera() {
    if let Some(call) = super::current() {
        stop(&call, MediaKind::Camera, false);
    }
}
/// Ends both publications without signaling (the attempt or call is over).
pub(super) fn release(call: &Call) {
    let mut publishing = call.publishing.borrow_mut();
    publishing.screen = None;
    publishing.camera = None;
    publishing_state(&publishing);
}

/// Moves the live camera to the saved selection, or with `flip` between the
/// front and back cameras. Replaces the track: no new camera epoch or
/// renegotiation. Waits for a switch already in progress.
pub async fn switch_camera(flip: bool) {
    clear_error();
    let call = loop {
        let Some(call) = super::current() else {
            return;
        };
        if !call.publishing.borrow().switching {
            break call;
        }
        gloo_timers::future::TimeoutFuture::new(100).await;
    };
    {
        let mut publishing = call.publishing.borrow_mut();
        if flip {
            publishing.facing = publishing.facing.flipped();
        }
        publishing.facing_only = flip;
        if publishing.camera.is_none() {
            return;
        }
        publishing.switching = true;
    }
    let result = camera_stream(&call).await;
    match result {
        Ok(stream) => {
            if call.publishing.borrow().camera.is_some() {
                publish(
                    Some(call.clone()),
                    MediaKind::Camera,
                    stream,
                    Quality::default(),
                )
                .await;
            } else {
                stop_stream(&stream);
            }
        }
        Err(error) => fail(media_error(MediaKind::Camera, &error).unwrap_or_default()),
    }
    call.publishing.borrow_mut().switching = false;
}

/// The local capture track for self-preview (video only).
pub fn local_track(kind: MediaKind) -> Option<MediaStreamTrack> {
    let call = super::current()?;
    let publishing = call.publishing.borrow();
    let capture = match kind {
        MediaKind::Camera => publishing.camera.as_ref(),
        MediaKind::ScreenVideo => publishing.screen.as_ref(),
        _ => None,
    }?;
    // `Clone::clone`: web-sys `MediaStreamTrack::clone` would create a JS
    // track clone that nobody stops.
    Some(Clone::clone(&capture.video))
}

/// One mounted remote player and its feedback state.
struct View {
    id: u32,
    watch: Watch,
    element: HtmlVideoElement,
    bound: Option<String>,
    budget: Budget,
    origin: f64,
    previous: Option<Inbound>,
    pressure: DecoderPressure,
    pressured: bool,
    recovery: Recovery,
    /// Scrolled out of a scrolling grid or strip (IntersectionObserver).
    off_screen: bool,
    /// A camera came back on screen; request a keyframe with the renewal.
    resumed: bool,
}
/// Registers a mounted player. The call binds the received track and keeps
/// the subscription alive while it is registered and the page is visible.
pub fn watch(element: HtmlVideoElement, watch: Watch) -> u32 {
    let id = NEXT_VIEW.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1).max(1));
        id
    });
    let maximum = if watch.kind == MediaKind::Camera {
        rules::CAMERA_MAX_BITRATE
    } else {
        thiscord_shared::screen::INITIAL_VIEW_BITRATE
    };
    VIEWS.with(|views| {
        views.borrow_mut().push(View {
            id,
            watch,
            element,
            bound: None,
            budget: Budget::with_maximum(maximum),
            origin: now(),
            previous: None,
            pressure: DecoderPressure::default(),
            pressured: false,
            recovery: Recovery::default(),
            off_screen: false,
            resumed: false,
        })
    });
    if let Some(call) = super::current() {
        bind(&call);
    }
    id
}
/// Unregisters a player; its subscription ends with the next renewal.
pub fn unwatch(id: u32) {
    VIEWS.with(|views| {
        views.borrow_mut().retain(|view| {
            if view.id == id {
                view.element.set_src_object(None);
                false
            } else {
                true
            }
        })
    });
}
/// Reported by the tile's IntersectionObserver.
pub fn set_off_screen(id: u32, off_screen: bool) {
    VIEWS.with(|views| {
        for view in views.borrow_mut().iter_mut().filter(|v| v.id == id) {
            view.resumed |= view.off_screen && !off_screen && view.watch.kind == MediaKind::Camera;
            view.off_screen = off_screen;
        }
    });
}
/// The player has a received track attached.
pub fn bound(id: u32) -> bool {
    VIEWS.with(|views| {
        views
            .borrow()
            .iter()
            .any(|view| view.id == id && view.bound.is_some())
    })
}

fn current_source<'a>(roster: &'a [Participant], watch: &Watch) -> Option<&'a Participant> {
    roster.iter().find(|m| {
        m.slot == watch.slot
            && m.account_id == watch.owner
            && m.video_active(watch.kind)
            && m.video_epoch(watch.kind) == watch.epoch
    })
}

/// Attaches received tracks to registered players of the current attempt.
fn bind(call: &Call) {
    let live = call.live.borrow();
    VIEWS.with(|views| {
        for view in views.borrow_mut().iter_mut() {
            let track = live.as_ref().and_then(|live| {
                live.media
                    .remote
                    .iter()
                    .find(|r| r.kind == view.watch.kind && r.slot == view.watch.slot)
            });
            let id = track.map(|r| r.track.id());
            if id == view.bound {
                continue;
            }
            view.bound = id;
            view.previous = None;
            view.recovery = Recovery::default();
            match track.and_then(|r| MediaStream::new_with_tracks(&Array::of1(&r.track)).ok()) {
                Some(stream) => {
                    view.element.set_muted(true);
                    view.element.set_src_object(Some(&stream));
                    if let Ok(promise) = view.element.play() {
                        wasm_bindgen_futures::spawn_local(async move {
                            let _ = JsFuture::from(promise).await;
                        });
                    }
                }
                None => view.element.set_src_object(None),
            }
        }
    });
}
/// Clears every player's track when an attempt ends.
pub(super) fn unbind_all() {
    VIEWS.with(|views| {
        for view in views.borrow_mut().iter_mut() {
            view.bound = None;
            view.element.set_src_object(None);
        }
    });
}

/// Inbound video counters keyed by `(mid, trackIdentifier)`.
pub(super) type InboundVideo = Vec<(Option<String>, Option<String>, Inbound)>;
pub(super) fn wants_stats() -> bool {
    VIEWS.with(|views| views.borrow().iter().any(|v| v.bound.is_some()))
}
/// One video `inbound-rtp` report, read during `media::stats`'s single walk.
pub(super) fn inbound_entry(stat: &JsValue, into: &mut InboundVideo) {
    if into.len() >= ROOM_CAPACITY * 2 {
        return;
    }
    let count = |key: &str| number(stat, key).unwrap_or(0.0).max(0.0) as u64;
    into.push((
        text(stat, "mid"),
        text(stat, "trackIdentifier"),
        Inbound {
            packets_received: count("packetsReceived"),
            packets_lost: count("packetsLost"),
            frames_received: count("framesReceived"),
            frames_decoded: count("framesDecoded"),
            decode_seconds: number(stat, "totalDecodeTime").unwrap_or(0.0),
        },
    ));
}

/// Applies `ScreenTarget`: the SFU's lowest viewer budget for our stream.
pub(super) fn target(call: &Call, kind: MediaKind, epoch: u32, bitrate: u32) {
    let current = call
        .live
        .borrow()
        .as_ref()
        .and_then(|live| live.own().map(|own| own.video_epoch(kind)))
        == Some(epoch);
    if !current {
        return;
    }
    {
        let mut publishing = call.publishing.borrow_mut();
        let capture = match kind {
            MediaKind::Camera => publishing.camera.as_mut(),
            MediaKind::ScreenVideo => publishing.screen.as_mut(),
            _ => None,
        };
        if let Some(capture) = capture {
            capture.target = Some(bitrate);
        }
    }
    apply_limits(call);
}
/// Writes changed encoder limits with `setParameters`, one at a time per sender.
fn apply_limits(call: &Call) {
    let senders = call
        .live
        .borrow()
        .as_ref()
        .map(|live| live.media.senders.clone())
        .unwrap_or_default();
    let mut guard = call.publishing.borrow_mut();
    let publishing = &mut *guard;
    for (kind, capture) in [
        (MediaKind::ScreenVideo, publishing.screen.as_mut()),
        (MediaKind::Camera, publishing.camera.as_mut()),
    ] {
        let Some(capture) = capture else {
            continue;
        };
        let Some(sender) = sender_index(kind).and_then(|i| senders[i].clone()) else {
            continue;
        };
        let maximum = rules::maximum_bitrate(kind, capture.quality);
        // Until the first target, send at the preset ceiling.
        let limit = rules::encoder_limit(capture.target.unwrap_or(maximum), maximum);
        if capture.applied == Some(limit) || capture.applying.get() {
            continue;
        }
        capture.applied = Some(limit);
        capture.applying.set(true);
        let applying = capture.applying.clone();
        let fps = if kind == MediaKind::Camera {
            rules::CAMERA_FPS
        } else {
            capture.quality.fps
        };
        wasm_bindgen_futures::spawn_local(async move {
            let parameters = sender.get_parameters();
            if let Ok(encodings) =
                Reflect::get(&parameters, &"encodings".into()).and_then(|e| e.dyn_into::<Array>())
                && encodings.length() > 0
            {
                for encoding in encodings.iter() {
                    let _ = Reflect::set(&encoding, &"active".into(), &limit.active.into());
                    let _ = Reflect::set(
                        &encoding,
                        &"maxBitrate".into(),
                        &f64::from(limit.max_bitrate).into(),
                    );
                    let _ = Reflect::set(&encoding, &"maxFramerate".into(), &f64::from(fps).into());
                }
                let _ = JsFuture::from(sender.set_parameters_with_parameters(&parameters)).await;
            }
            applying.set(false);
        });
    }
}

/// Per-tick work: unpublish ended or disallowed media, bind players, renew
/// viewing demand, sample viewer feedback and request keyframes.
pub(super) fn tick(call: &Call, stats: Option<InboundVideo>) {
    let ended: Vec<MediaKind> = {
        let publishing = call.publishing.borrow();
        [
            (MediaKind::ScreenVideo, &publishing.screen),
            (MediaKind::Camera, &publishing.camera),
        ]
        .into_iter()
        .filter(|(kind, _)| *kind != MediaKind::Camera || !publishing.switching)
        .filter(|(_, c)| c.as_ref().is_some_and(|c| !c.live()))
        .map(|(kind, _)| kind)
        .collect()
    };
    for kind in ended {
        stop(call, kind, true);
    }
    for kind in [MediaKind::ScreenVideo, MediaKind::Camera] {
        let publishing = match kind {
            MediaKind::Camera => call.publishing.borrow().camera.is_some(),
            _ => call.publishing.borrow().screen.is_some(),
        };
        // Deafen, server mute/deafen or lost Speak stop local capture too.
        if publishing && blocked(call, kind).is_some() {
            stop(call, kind, true);
        }
    }
    apply_limits(call);
    bind(call);
    let time = now();
    let mut keyframes = Vec::new();
    let views = {
        let live = call.live.borrow();
        let Some(live) = live.as_ref().filter(|l| l.answered()) else {
            return;
        };
        let own_slot = live.own().map(|m| m.slot);
        let shown = visible();
        VIEWS.with(|views| {
            let mut views = views.borrow_mut();
            if let Some(stats) = &stats {
                for view in views.iter_mut().filter(|v| v.bound.is_some()) {
                    let Some(remote) = live
                        .media
                        .remote
                        .iter()
                        .find(|r| r.kind == view.watch.kind && r.slot == view.watch.slot)
                    else {
                        continue;
                    };
                    let track = remote.track.id();
                    let Some((_, _, current)) = stats.iter().find(|(mid, id, _)| {
                        mid.as_deref() == Some(remote.mid.as_str())
                            || id.as_deref() == Some(track.as_str())
                    }) else {
                        continue;
                    };
                    let current = *current;
                    let previous = view.previous.unwrap_or_default();
                    view.pressured |= view.pressure.sample(DecoderSample {
                        at_ms: time,
                        received: current.frames_received,
                        decoded: current.frames_decoded,
                        decode_seconds: current.decode_seconds,
                    });
                    if view.previous.is_some() {
                        view.budget.sample_at(
                            rules::receiver_sample(previous, current, view.pressured),
                            std::time::Duration::from_secs_f64(
                                ((time - view.origin) / 1000.0).max(0.0),
                            ),
                        );
                        view.pressured = false;
                    }
                    let arrived = current.packets_received > previous.packets_received;
                    if shown && view.recovery.due(current, arrived, time) {
                        keyframes.push(view.watch);
                    }
                    view.previous = Some(current);
                }
            }
            views
                .iter()
                .filter(|v| shown && v.bound.is_some() && Some(v.watch.slot) != own_slot)
                // A camera scrolled out of view stops downloading and decoding. A
                // screen keeps its subscription: it carries shared audio, and a
                // publisher without layers is encoded for its lowest viewer budget.
                .filter(|v| !(v.off_screen && v.watch.kind == MediaKind::Camera))
                .filter(|v| current_source(&live.roster, &v.watch).is_some())
                .map(|v| Subscription {
                    kind: v.watch.kind,
                    slot: v.watch.slot,
                    owner: v.watch.owner,
                    epoch: v.watch.epoch,
                    bitrate: v.budget.bitrate(),
                    h264_high: false,
                })
                .fold(Vec::<Subscription>::new(), |mut list, view| {
                    // One subscription per stream, even with duplicate players;
                    // an on-screen duplicate keeps an off-screen one's stream sharp.
                    match list
                        .iter_mut()
                        .find(|s| s.kind == view.kind && s.slot == view.slot)
                    {
                        Some(existing) => existing.bitrate = existing.bitrate.max(view.bitrate),
                        None => list.push(view),
                    }
                    list
                })
        })
    };
    let mut live = call.live.borrow_mut();
    let Some(live) = live.as_mut() else {
        return;
    };
    // Shared screen audio plays only while its screen is watched; the SFU
    // also stops forwarding it to non-viewers with the next renewal.
    let media = &mut live.media;
    for (slot, track) in &media.shared_audio {
        let wanted = views
            .iter()
            .any(|v| v.kind == MediaKind::ScreenVideo && v.slot == *slot);
        let stopped = &mut media.audio_stopped[*slot % ROOM_CAPACITY];
        if wanted && *stopped {
            call.output
                .play_remote(RemoteSource::SystemAudio, *slot, track);
        } else if !wanted && !*stopped {
            call.output.stop_remote(RemoteSource::SystemAudio, *slot);
        }
        *stopped = !wanted;
    }
    for watch in keyframes {
        live.send(ClientEvent::ScreenKeyframe {
            kind: watch.kind,
            slot: watch.slot,
            epoch: watch.epoch,
        });
    }
    if rules::views_due(
        views != live.media.sent_views,
        views.is_empty(),
        time - live.media.views_at,
    ) {
        live.media.views_at = time;
        live.media.sent_views = views.clone();
        live.send(ClientEvent::ScreenViews { views });
        // The SFU honors keyframe requests only from subscribed viewers, so a
        // camera that scrolled back asks for one after its renewed view.
        let resumed: Vec<Watch> = VIEWS.with(|views| {
            views
                .borrow_mut()
                .iter_mut()
                .filter(|v| v.resumed && v.bound.is_some())
                .map(|v| {
                    v.resumed = false;
                    v.watch
                })
                .collect()
        });
        for watch in resumed {
            live.send(ClientEvent::ScreenKeyframe {
                kind: watch.kind,
                slot: watch.slot,
                epoch: watch.epoch,
            });
        }
    }
}
/// Whether the next tick should sample inbound video statistics.
pub(super) fn stats_due(call: &Call) -> bool {
    let mut live = call.live.borrow_mut();
    let Some(live) = live.as_mut() else {
        return false;
    };
    if !wants_stats() || now() - live.media.stats_at < rules::FEEDBACK_MS {
        return false;
    }
    live.media.stats_at = now();
    true
}
/// Publication state without subscribing the caller.
pub fn state_untracked() -> State {
    STATE.with(|state| state.get_untracked())
}
/// Shows a screen-share or camera problem in the voice bar.
pub fn show_error(message: &str) {
    fail(message);
}
