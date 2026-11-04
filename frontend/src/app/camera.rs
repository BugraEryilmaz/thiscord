//! Rust/WASM controls WebView camera capture and its local encrypted video bridge.
use super::Ui;
use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    MediaStream, MediaStreamTrack, MediaStreamTrackState, RtcIceGatheringState, RtcPeerConnection,
    RtcPeerConnectionState, RtcRtpSender, RtcRtpTransceiverDirection, RtcRtpTransceiverInit,
    RtcSdpType, RtcSessionDescriptionInit,
};
static NEXT_SESSION: AtomicU32 = AtomicU32::new(1);
#[derive(Default)]
struct LocalPreview {
    stream: Option<MediaStream>,
    viewers: Vec<(u32, web_sys::HtmlVideoElement)>,
}
thread_local! { static LOCAL: RefCell<LocalPreview> = RefCell::new(LocalPreview::default()); }
fn local_stream(stream: Option<MediaStream>) {
    LOCAL.with(|local| {
        let mut local = local.borrow_mut();
        for (_, video) in &local.viewers {
            video.set_src_object(stream.as_ref());
            if stream.is_some() {
                let _ = video.play();
            }
        }
        local.stream = stream;
    });
}
struct Capture {
    /// Replaced in place when the camera is switched during a call.
    stream: RefCell<MediaStream>,
    pc: RtcPeerConnection,
    session: u32,
    stopped: Cell<bool>,
    /// The camera selection (`cameras::revision`) this capture follows.
    revision: Cell<u32>,
}
impl Capture {
    fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        crate::cameras::stop_stream(&self.stream.borrow());
        self.pc.close();
        let session = self.session;
        leptos::task::spawn_local(async move {
            let _ = native::<()>("camera_stop", json!({"session":session})).await;
        });
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}
fn media_error(error: JsValue) -> String {
    crate::cameras::error_message(&error).into()
}
async fn capture(session: u32, generation: &Cell<u32>) -> Result<Capture, String> {
    native::<()>("camera_prepare", json!({"session":session})).await?;
    if generation.get() != session {
        let _ = native::<()>("camera_stop", json!({"session":session})).await;
        return Err("Camera start cancelled".into());
    }
    // The saved camera, or the system default with a one-time notice.
    let revision = crate::cameras::revision_untracked();
    let stream = crate::cameras::open(None, true)
        .await
        .map_err(media_error)?;
    let pc = match RtcPeerConnection::new() {
        Ok(pc) => pc,
        Err(_) => {
            crate::cameras::stop_stream(&stream);
            return Err("WebRTC camera support is unavailable".into());
        }
    };
    Ok(Capture {
        stream: RefCell::new(stream),
        pc,
        session,
        stopped: Cell::new(false),
        revision: Cell::new(revision),
    })
}
fn json_value(value: serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::UNDEFINED)
}
async fn connect(capture: &Capture) -> Result<(), String> {
    let track: MediaStreamTrack = capture
        .stream
        .borrow()
        .get_video_tracks()
        .get(0)
        .dyn_into()
        .map_err(|_| crate::i18n::t("No camera video track"))?;
    let init = RtcRtpTransceiverInit::new();
    init.set_direction(RtcRtpTransceiverDirection::Sendonly);
    init.set_send_encodings(&js_sys::Array::of1(&json_value(
        json!({"maxBitrate":1_500_000,"maxFramerate":30}),
    )));
    capture
        .pc
        .add_transceiver_with_media_stream_track_and_init(&track, &init);
    let offer: RtcSessionDescriptionInit = JsFuture::from(capture.pc.create_offer())
        .await
        .map_err(|_| crate::i18n::t("Cannot create camera offer"))?
        .unchecked_into();
    JsFuture::from(capture.pc.set_local_description(&offer))
        .await
        .map_err(|_| crate::i18n::t("Cannot initialize camera"))?;
    for _ in 0..100 {
        if capture.pc.ice_gathering_state() == RtcIceGatheringState::Complete {
            break;
        }
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
    if capture.pc.ice_gathering_state() != RtcIceGatheringState::Complete {
        return Err("Camera connection timed out".into());
    }
    let answer: thiscord_shared::screen::ViewAnswer = native("camera_open",json!({"session":capture.session,"sdp":capture.pc.local_description().ok_or(crate::i18n::t("Missing camera offer"))?.sdp()})).await?;
    let description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    description.set_sdp(&answer.sdp);
    JsFuture::from(capture.pc.set_remote_description(&description))
        .await
        .map_err(|_| crate::i18n::t("Cannot connect camera"))?;
    for _ in 0..20 {
        if capture.pc.connection_state() == RtcPeerConnectionState::Connected {
            return Ok(());
        }
        if matches!(
            capture.pc.connection_state(),
            RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed
        ) {
            return Err("Camera transport closed".into());
        }
        let status: thiscord_shared::camera::Status =
            native("camera_keepalive", json!({"session":capture.session})).await?;
        if !status.alive {
            return Err("Camera start cancelled".into());
        }
        gloo_timers::future::TimeoutFuture::new(500).await;
    }
    Err("Camera connection timed out".into())
}
async fn budget(capture: &Capture, bitrate: u32) -> Result<(), String> {
    for sender in capture.pc.get_senders().iter() {
        let sender: web_sys::RtcRtpSender = sender.unchecked_into();
        let parameters = sender.get_parameters();
        if let Some(encodings) = parameters.get_encodings() {
            for encoding in encodings.iter() {
                js_sys::Reflect::set(
                    &encoding,
                    &"active".into(),
                    &JsValue::from_bool(bitrate > 0),
                )
                .map_err(|_| crate::i18n::t("Cannot pause camera encoding"))?;
                js_sys::Reflect::set(
                    &encoding,
                    &"maxBitrate".into(),
                    &JsValue::from_f64(f64::from(bitrate.max(200_000))),
                )
                .map_err(|_| crate::i18n::t("Cannot limit camera bitrate"))?;
            }
            JsFuture::from(sender.set_parameters_with_parameters(&parameters))
                .await
                .map_err(|_| crate::i18n::t("Cannot apply camera bitrate limit"))?;
        }
    }
    Ok(())
}
/// Moves a running capture to the currently selected camera by replacing the
/// bridge sender's track: same session, camera epoch and viewer leases, no
/// renegotiation. On failure the previous camera keeps publishing.
async fn switch(capture: &Capture) -> Result<(), String> {
    let stream = crate::cameras::open(None, true)
        .await
        .map_err(media_error)?;
    if capture.stopped.get() {
        crate::cameras::stop_stream(&stream);
        return Ok(());
    }
    let Ok(track) = stream
        .get_video_tracks()
        .get(0)
        .dyn_into::<MediaStreamTrack>()
    else {
        crate::cameras::stop_stream(&stream);
        return Err(crate::i18n::t("No camera video track").into());
    };
    for sender in capture.pc.get_senders().iter() {
        let sender: RtcRtpSender = sender.unchecked_into();
        if JsFuture::from(sender.replace_track(Some(&track)))
            .await
            .is_err()
        {
            crate::cameras::stop_stream(&stream);
            return Err(crate::i18n::t("Cannot switch camera").into());
        }
    }
    // A stop during the switch already stopped the previous stream.
    if capture.stopped.get() {
        crate::cameras::stop_stream(&stream);
        return Ok(());
    }
    let previous = capture.stream.replace(stream.clone());
    crate::cameras::stop_stream(&previous);
    local_stream(Some(stream));
    Ok(())
}
#[component]
pub(super) fn CameraControls(ui: Ui) -> impl IntoView {
    let active = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let generation = Rc::new(std::cell::Cell::new(0u32));
    let owned_session = Rc::new(Cell::new(None));
    let current: Rc<RefCell<Option<Rc<Capture>>>> = Rc::new(RefCell::new(None));
    // A camera switch is replacing the track; WebKit may end the previous
    // track as soon as another camera opens, which is not a device failure.
    let switching = Rc::new(Cell::new(false));
    Effect::new(move |_| crate::cameras::set_call_camera(active.get()));
    let stop = {
        let current = current.clone();
        let generation = generation.clone();
        let owned_session = owned_session.clone();
        Rc::new(move || {
            generation.set(generation.get().wrapping_add(1));
            if let Some(c) = current.borrow_mut().take() {
                c.stop();
            }
            local_stream(None);
            // Cleanup may run after `active` is disposed.
            crate::cameras::set_call_camera(false);
            // Also release a reservation while getUserMedia/permission is pending.
            if let Some(session) = owned_session.take() {
                leptos::task::spawn_local(async move {
                    let _ = native::<()>("camera_stop", json!({"session":session})).await;
                });
            }
            if active.try_get_untracked().is_some() {
                active.set(false);
                busy.set(false);
            }
        })
    };
    let cleanup = StoredValue::new_local(stop.clone());
    on_cleanup(move || cleanup.with_value(|f| f()));
    let end = stop.clone();
    Effect::new(move |_| {
        if !super::audio::media_publish_allowed(ui) {
            end();
        }
    });
    // Follow camera selections (voice-bar menu or settings) while publishing.
    let follow = {
        let current = current.clone();
        let switching = switching.clone();
        Rc::new(move || {
            if switching.replace(true) {
                return;
            }
            let current = current.clone();
            let switching = switching.clone();
            leptos::task::spawn_local(async move {
                loop {
                    let Some(capture) = current.borrow().clone() else {
                        break;
                    };
                    let wanted = crate::cameras::revision_untracked();
                    if capture.stopped.get()
                        || capture.revision.get() == wanted
                        || !active.try_get_untracked().unwrap_or(false)
                    {
                        break;
                    }
                    let result = switch(&capture).await;
                    // Failed choices are not retried until the next selection.
                    capture.revision.set(wanted);
                    if let Err(e) = result
                        && !capture.stopped.get()
                    {
                        error.try_set(e);
                        break;
                    }
                }
                switching.set(false);
            });
        })
    };
    let follow_selection = follow.clone();
    Effect::new(move |_| {
        crate::cameras::revision();
        follow_selection();
    });
    let toggle = move |_| {
        if !crate::account_client::desktop() {
            error.set("Open the desktop app to use your camera".into());
            return;
        }
        if active.get_untracked() || busy.get_untracked() {
            stop();
            return;
        }
        let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        owned_session.set(Some(session));
        generation.set(session);
        busy.set(true);
        error.set(String::new());
        let generation = generation.clone();
        let current = current.clone();
        let stop = stop.clone();
        let switching = switching.clone();
        let follow = follow.clone();
        leptos::task::spawn_local(async move {
            let result = capture(session, &generation).await;
            if generation.get() != session {
                drop(result);
                return;
            }
            let result = match result {
                Ok(c) => {
                    let c = Rc::new(c);
                    local_stream(Some(c.stream.borrow().clone()));
                    *current.borrow_mut() = Some(c.clone());
                    match connect(&c).await {
                        Ok(()) => Ok(c),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            };
            if generation.get() != session {
                drop(result);
                return;
            }
            busy.set(false);
            match result {
                Err(e) => {
                    stop();
                    error.set(e);
                }
                Ok(c) => {
                    local_stream(Some(c.stream.borrow().clone()));
                    *current.borrow_mut() = Some(c);
                    active.set(true);
                    // The selection may have changed while connecting.
                    follow();
                    let mut last_budget = None;
                    let mut disconnected = 0;
                    loop {
                        gloo_timers::future::TimeoutFuture::new(500).await;
                        if generation.get() != session {
                            break;
                        }
                        let live = current.borrow().as_ref().is_some_and(|c| {
                            !matches!(
                                c.pc.connection_state(),
                                RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed
                            ) && (switching.get()
                                || c.stream.borrow().get_video_tracks().iter().all(|t| {
                                    t.unchecked_into::<MediaStreamTrack>().ready_state()
                                        == MediaStreamTrackState::Live
                                }))
                        });
                        let allowed = native::<thiscord_shared::camera::Status>(
                            "camera_keepalive",
                            json!({"session":session}),
                        )
                        .await
                        .unwrap_or_default();
                        if generation.get() != session {
                            break;
                        }
                        let capture = current.borrow().clone();
                        if !live || !allowed.alive {
                            stop();
                            error.set(
                                "Camera stopped. Check the device and your call connection.".into(),
                            );
                            break;
                        }
                        if let Some(capture) = capture {
                            if capture.pc.connection_state() == RtcPeerConnectionState::Connected {
                                disconnected = 0;
                            } else {
                                disconnected += 1;
                            }
                            if last_budget != Some(allowed.bitrate) {
                                if let Err(e) = budget(&capture, allowed.bitrate).await {
                                    if generation.get() == session {
                                        stop();
                                        error.set(e);
                                    }
                                    break;
                                }
                                last_budget = Some(allowed.bitrate);
                            }
                        }
                        if generation.get() != session {
                            break;
                        }
                        if disconnected >= 20 {
                            stop();
                            error.set(
                                "Camera stopped. Check the device and your call connection.".into(),
                            );
                            break;
                        }
                    }
                }
            }
        });
    };
    view! {
        <button class="icon-button" class:active=move ||active.get() disabled=move ||!crate::account_client::desktop() || !super::audio::media_publish_allowed(ui)
            aria-label=move ||if busy.get(){crate::i18n::t("Cancel camera")}else{crate::i18n::t("Camera")}
            data-tip=move ||if !crate::account_client::desktop(){crate::i18n::t("Available in the desktop app")}else if active.get(){crate::i18n::t("Turn camera off")}else{crate::i18n::t("Turn camera on")} aria-pressed=move ||active.get() on:click=toggle>
            {move ||view!{<super::icons::Icon kind=if active.get(){super::icons::Glyph::Camera}else{super::icons::Glyph::CameraOff}/>}}
        </button>
        <super::camera_select::CameraMenu ui=ui/>
        <Show when=move ||!error.get().is_empty()><span class="max-w-64 text-xs text-red-300" role="alert">{move ||crate::i18n::message(error.get())}</span></Show>
    }
}
#[component]
pub(super) fn LocalCamera() -> impl IntoView {
    let node = NodeRef::<leptos::html::Video>::new();
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    Effect::new(move |_| {
        if let Some(video) = node.get() {
            let video: web_sys::HtmlVideoElement = video.clone();
            LOCAL.with(|local| {
                let mut local = local.borrow_mut();
                video.set_src_object(local.stream.as_ref());
                video.set_muted(true);
                let _ = video.play();
                local.viewers.retain(|(key, _)| *key != id);
                local.viewers.push((id, video));
            });
        }
    });
    on_cleanup(move || {
        LOCAL.with(|local| {
            local.borrow_mut().viewers.retain(|(key, video)| {
                if *key == id {
                    video.set_src_object(None);
                    false
                } else {
                    true
                }
            })
        })
    });
    view! {<div class="stream-player"><video node_ref=node autoplay muted playsinline aria-label=move || crate::i18n::t("Your camera")/></div>}
}
