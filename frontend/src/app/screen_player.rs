//! Browser-native H.264 decoding/compositing. Rust/WASM only controls signaling.
use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use thiscord_frontend::{
    video_bridge::{self as bridge, Pair},
    video_feedback::{DecoderPressure, DecoderSample},
};
use thiscord_shared::{
    screen::{ViewAnswer, Watch},
    video_bridge::{Kind, Outcome, Update},
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;
use web_sys::*;

/// The WebView half of one bridge record. The desktop merges it with its own
/// half for remote support. Facts are classified scalars, never SDP or addresses.
#[derive(Clone)]
struct Trace(Rc<RefCell<(Update, f64)>>);
impl Trace {
    fn new(kind: Kind) -> Self {
        // Below 2^53 so the ID survives JavaScript numbers; zero means untracked.
        let id = (js_sys::Math::random() * 9_007_199_254_740_990.0) as u64 + 1;
        let trace = Self(Rc::new(RefCell::new((
            Update {
                id,
                kind,
                outcome: Outcome::Pending,
                webview: Default::default(),
            },
            js_sys::Date::now(),
        ))));
        if let Some(agent) = web_sys::window().and_then(|w| w.navigator().user_agent().ok()) {
            trace.fact("user_agent", agent);
        }
        trace
    }
    fn id(&self) -> u64 {
        self.0.borrow().0.id
    }
    fn event(&self, text: impl Into<String>) {
        let mut inner = self.0.borrow_mut();
        let at = (js_sys::Date::now() - inner.1).max(0.0) as u64;
        inner.0.webview.event(at, text);
    }
    fn fact(&self, key: &str, value: impl ToString) {
        self.0.borrow_mut().0.webview.fact(key, value);
    }
    fn finish(&self, outcome: Outcome, error: Option<&str>) {
        if let Some(error) = error {
            self.fact(bridge::ERROR, error);
            self.event(format!("failed: {error}"));
        }
        self.0.borrow_mut().0.outcome = outcome;
    }
    /// Sends the current state to the desktop; diagnostics never block playback.
    async fn push(&self, pc: Option<&RtcPeerConnection>) {
        if let Some(pc) = pc {
            let facts = bridge_stats(pc).await;
            bridge::apply_stats(&mut self.0.borrow_mut().0.webview, &facts);
        }
        let update = self.0.borrow().0.clone();
        let _ = native::<()>("screen_bridge_report", json!({ "update": update })).await;
    }
}

/// DOM listeners owned by Rust and removed with their player.
struct Listeners {
    target: EventTarget,
    names: &'static [&'static str],
    callback: Closure<dyn FnMut(Event)>,
}
impl Listeners {
    fn new(
        target: &EventTarget,
        names: &'static [&'static str],
        f: impl FnMut(Event) + 'static,
    ) -> Self {
        let callback = Closure::<dyn FnMut(Event)>::new(f);
        for name in names {
            let _ =
                target.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
        }
        Self {
            target: target.clone(),
            names,
            callback,
        }
    }
}
impl Drop for Listeners {
    fn drop(&mut self) {
        for name in self.names {
            let _ = self
                .target
                .remove_event_listener_with_callback(name, self.callback.as_ref().unchecked_ref());
        }
    }
}
fn property(object: &JsValue, name: &str) -> String {
    let value =
        js_sys::Reflect::get(object, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED);
    value
        .as_string()
        .or_else(|| value.as_f64().map(|n| n.to_string()))
        .or_else(|| value.as_bool().map(|b| b.to_string()))
        .unwrap_or_default()
}
fn watch_peer(pc: &RtcPeerConnection, trace: &Trace) -> Listeners {
    let (peer, trace) = (pc.clone(), trace.clone());
    Listeners::new(
        pc,
        &[
            "iceconnectionstatechange",
            "connectionstatechange",
            "icegatheringstatechange",
            "signalingstatechange",
            "icecandidateerror",
        ],
        move |event: Event| {
            let (label, field) = match event.type_().as_str() {
                "iceconnectionstatechange" => ("ice", "iceConnectionState"),
                "connectionstatechange" => ("connection", "connectionState"),
                "icegatheringstatechange" => ("gathering", "iceGatheringState"),
                "signalingstatechange" => ("signaling", "signalingState"),
                // The failing address and STUN URL are deliberately omitted.
                _ => {
                    trace.event(format!(
                        "candidate error {} {}",
                        property(&event, "errorCode"),
                        property(&event, "errorText")
                    ));
                    return;
                }
            };
            trace.event(format!("{label}={}", property(&peer, field)));
        },
    )
}

struct Player {
    pc: RtcPeerConnection,
    video: HtmlVideoElement,
    _track: Closure<dyn FnMut(RtcTrackEvent)>,
    _peer_events: Listeners,
    _video_events: Listeners,
}
impl Drop for Player {
    fn drop(&mut self) {
        self.pc.set_ontrack(None);
        self.pc.close();
        self.video.set_src_object(None);
    }
}
/// Closes self-test peers on every exit path.
struct Closing(RtcPeerConnection);
impl Drop for Closing {
    fn drop(&mut self) {
        self.0.close();
    }
}
fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| !d.hidden())
}
/// Creates and fully gathers an offer, recording what the WebView can offer.
async fn offer(pc: &RtcPeerConnection, trace: &Trace) -> Result<String, String> {
    let started = js_sys::Date::now();
    let offer = JsFuture::from(pc.create_offer())
        .await
        .map_err(|_| crate::i18n::t("Cannot create video player"))?;
    let offer: RtcSessionDescriptionInit = offer.unchecked_into();
    JsFuture::from(pc.set_local_description(&offer))
        .await
        .map_err(|_| crate::i18n::t("Cannot initialize video player"))?;
    // Only connection setup uses a timer; frames flow through WebRTC directly.
    for _ in 0..100 {
        if pc.ice_gathering_state() == RtcIceGatheringState::Complete {
            break;
        }
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
    trace.fact(
        "timing.gathering_ms",
        (js_sys::Date::now() - started) as u64,
    );
    let sdp = pc.local_description().map(|d| d.sdp());
    if let Some(sdp) = &sdp {
        let (candidates, udp) = bridge::candidate_summary(sdp);
        trace.fact(bridge::OFFER_CANDIDATES, candidates);
        trace.fact(bridge::OFFER_UDP, udp);
        trace.fact("offer.codecs", bridge::codec_summary(sdp));
    }
    if pc.ice_gathering_state() != RtcIceGatheringState::Complete {
        return Err("Video player connection timed out".into());
    }
    sdp.ok_or_else(|| crate::i18n::t("Missing video offer").into())
}
async fn accept(pc: &RtcPeerConnection, trace: &Trace, sdp: &str) -> Result<(), String> {
    trace.fact("answer.candidates", bridge::candidate_summary(sdp).0);
    let remote = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    remote.set_sdp(sdp);
    JsFuture::from(pc.set_remote_description(&remote))
        .await
        .map_err(|_| crate::i18n::t("Cannot connect video player"))?;
    trace.event("answer applied");
    Ok(())
}
async fn connect(video: HtmlVideoElement, watch: Watch, trace: &Trace) -> Result<Player, String> {
    let pc = RtcPeerConnection::new()
        .map_err(|_| crate::i18n::t("This WebView cannot play WebRTC video"))?;
    let target = video.clone();
    let track = Closure::<dyn FnMut(RtcTrackEvent)>::new(move |event: RtcTrackEvent| {
        // A 4K IDR can take ~100 ms to pace even on a healthy link. The local
        // bridge forwards complete frames in bursts. Let the native player
        // smooth those bursts without interpreting presentation skips as decode
        // overload. This is a bounded latency target, not an extra encoded queue.
        let receiver = event.receiver();
        let property = JsValue::from_str("jitterBufferTarget");
        if js_sys::Reflect::has(receiver.as_ref(), &property).unwrap_or(false) {
            let _ = js_sys::Reflect::set(receiver.as_ref(), &property, &JsValue::from_f64(150.0));
        }
        if let Ok(stream) = MediaStream::new() {
            stream.add_track(&event.track());
            target.set_muted(true);
            target.set_src_object(Some(&stream));
            let _ = target.play();
        }
    });
    pc.set_ontrack(Some(track.as_ref().unchecked_ref()));
    let element = video.clone();
    let events = trace.clone();
    let player = Player {
        _peer_events: watch_peer(&pc, trace),
        _video_events: Listeners::new(
            &video,
            &[
                "loadedmetadata",
                "loadeddata",
                "playing",
                "waiting",
                "stalled",
                "error",
                "emptied",
            ],
            move |event: Event| {
                let detail = match event.type_().as_str() {
                    "loadedmetadata" => {
                        format!(" {}x{}", element.video_width(), element.video_height())
                    }
                    "error" => format!(
                        " code {}",
                        property(&property_value(&element, "error"), "code")
                    ),
                    _ => String::new(),
                };
                events.event(format!("video {}{detail}", event.type_()));
            },
        ),
        pc,
        video,
        _track: track,
    };
    let init = RtcRtpTransceiverInit::new();
    init.set_direction(RtcRtpTransceiverDirection::Recvonly);
    player.pc.add_transceiver_with_str_and_init("video", &init);
    let sdp = offer(&player.pc, trace).await?;
    let answer: ViewAnswer = native(
        "screen_view_open",
        json!({"offer":{"watch":watch,"sdp":sdp,"attempt":trace.id()}}),
    )
    .await?;
    accept(&player.pc, trace, &answer.sdp).await?;
    Ok(player)
}
fn property_value(object: &JsValue, name: &str) -> JsValue {
    js_sys::Reflect::get(object, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

pub async fn run(
    video: HtmlVideoElement,
    watch: Watch,
    message: RwSignal<String>,
    diagnostics: RwSignal<String>,
) {
    let kind = if watch.kind == thiscord_shared::voice::MediaKind::Camera {
        Kind::Camera
    } else {
        Kind::Screen
    };
    loop {
        while !visible() {
            gloo_timers::future::TimeoutFuture::new(250).await;
        }
        let trace = Trace::new(kind);
        match connect(video.clone(), watch, &trace).await {
            Ok(player) => {
                let mut pending = 0;
                let mut previous = thiscord_shared::screen::Diagnostics::default();
                let mut last_stats = js_sys::Date::now();
                let mut last_presented = 0;
                let mut last_pli = 0;
                let mut decoder_load = DecoderPressure::default();
                let mut decoder_pressure = false;
                let mut reached = Outcome::Pending;
                let mut ended = None;
                let mut last_push = js_sys::Date::now();
                trace.push(Some(&player.pc)).await;
                while visible() {
                    let state = player.pc.connection_state();
                    if matches!(
                        state,
                        RtcPeerConnectionState::Closed | RtcPeerConnectionState::Failed
                    ) {
                        ended = Some((Outcome::Failed, "player connection closed or failed"));
                        break;
                    }
                    if state == RtcPeerConnectionState::Connected {
                        let playing =
                            player.video.ready_state() >= HtmlMediaElement::HAVE_CURRENT_DATA;
                        message.set(if playing {
                            String::new()
                        } else {
                            "Waiting for video…".into()
                        });
                        pending = 0;
                        let now = if playing {
                            Outcome::Playing
                        } else {
                            Outcome::Connected
                        };
                        if reached != Outcome::Playing && now != reached {
                            reached = now;
                            trace.finish(now, None);
                            trace.event(if playing {
                                "first frame ready"
                            } else {
                                "player connected"
                            });
                            trace.push(Some(&player.pc)).await;
                            last_push = js_sys::Date::now();
                        }
                    } else {
                        pending += 1;
                        if pending > 40 {
                            ended = Some((
                                Outcome::TimedOut,
                                "player did not connect within 10 seconds",
                            ));
                            break;
                        }
                    }
                    if !native::<bool>(
                        "screen_view_keepalive",
                        json!({"watch":watch,"visible":true,"decoderPressure":decoder_pressure}),
                    )
                    .await
                    .unwrap_or(false)
                    {
                        ended = Some((Outcome::Closed, "desktop ended the viewer lease"));
                        break;
                    }
                    decoder_pressure = false;
                    let now = js_sys::Date::now();
                    if now - last_stats >= 1000.0 {
                        let seconds = (now - last_stats) / 1000.0;
                        last_stats = now;
                        if let Ok(stats) = native::<thiscord_shared::screen::Diagnostics>(
                            "screen_view_diagnostics",
                            json!({"watch":watch}),
                        )
                        .await
                        {
                            let (browser, pli, decoder) = browser_stats(&player.pc).await;
                            if pli > last_pli {
                                let _ =
                                    native::<bool>("screen_view_keyframe", json!({"watch":watch}))
                                        .await;
                            }
                            decoder_pressure = decoder.is_some_and(|s| decoder_load.sample(s));
                            last_pli = pli;
                            let playback = {
                                let q = player.video.get_video_playback_quality();
                                let presented = q
                                    .total_video_frames()
                                    .saturating_sub(q.dropped_video_frames());
                                let fps = presented.saturating_sub(last_presented) as f64
                                    / seconds.max(0.001);
                                last_presented = presented;
                                format!(
                                    "Presented FPS: {fps:.1}; total={}, dropped={}\n",
                                    q.total_video_frames(),
                                    q.dropped_video_frames()
                                )
                            };
                            diagnostics.set(format!(
                                "{}\n{}{}",
                                describe(&stats, &previous),
                                playback,
                                browser
                            ));
                            previous = stats;
                        }
                    }
                    let cadence = if reached == Outcome::Playing {
                        30_000.0
                    } else {
                        2_000.0
                    };
                    if js_sys::Date::now() - last_push >= cadence {
                        video_facts(&trace, &player.video);
                        trace.push(Some(&player.pc)).await;
                        last_push = js_sys::Date::now();
                    }
                    gloo_timers::future::TimeoutFuture::new(250).await;
                }
                // An attempt that played or connected keeps that result; the
                // timeline records why it ended.
                let (outcome, reason) = ended.unwrap_or((Outcome::Closed, "document hidden"));
                let outcome = if reached == Outcome::Pending {
                    outcome
                } else {
                    reached
                };
                trace.finish(outcome, None);
                trace.event(reason);
                video_facts(&trace, &player.video);
                trace.push(Some(&player.pc)).await;
                drop(player);
                if matches!(outcome, Outcome::TimedOut | Outcome::Failed) {
                    leptos::task::spawn_local(self_test());
                }
            }
            Err(error) => {
                trace.finish(Outcome::Error, Some(&error));
                trace.push(None).await;
                message.set(error);
            }
        }
        let _ = native::<bool>(
            "screen_view_keepalive",
            json!({"watch":watch,"visible":false}),
        )
        .await;
        gloo_timers::future::TimeoutFuture::new(1000).await;
    }
}
fn video_facts(trace: &Trace, video: &HtmlVideoElement) {
    trace.fact("video.ready_state", video.ready_state());
    trace.fact("video.network_state", video.network_state());
    trace.fact(
        "video.size",
        format!("{}x{}", video.video_width(), video.video_height()),
    );
    trace.fact("video.paused", video.paused());
    let quality = video.get_video_playback_quality();
    trace.fact(
        "video.frames",
        format!(
            "total {} dropped {}",
            quality.total_video_frames(),
            quality.dropped_video_frames()
        ),
    );
}

async fn wait_connected(pc: &RtcPeerConnection) -> bool {
    for _ in 0..80 {
        match pc.connection_state() {
            RtcPeerConnectionState::Connected => return true,
            RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed => return false,
            _ => gloo_timers::future::TimeoutFuture::new(100).await,
        }
    }
    false
}

thread_local! {
    static SELF_TEST_AT: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::NEG_INFINITY) };
}
/// After a failed attempt, at most every ten minutes, separate the WebView's
/// own WebRTC from the desktop bridge and try the bridge over network
/// interfaces. About 30 seconds, invisible and video-free.
async fn self_test() {
    let now = js_sys::Date::now();
    if now - SELF_TEST_AT.with(|at| at.get()) < 600_000.0 {
        return;
    }
    SELF_TEST_AT.with(|at| at.set(now));
    webview_self_test().await;
    for interfaces in [false, true] {
        desktop_self_test(interfaces).await;
    }
}
async fn webview_self_test() {
    let trace = Trace::new(Kind::WebViewSelfTest);
    let result = async {
        let fail = |_| "Cannot create WebRTC peers".to_owned();
        let a = Closing(RtcPeerConnection::new().map_err(fail)?);
        let b = Closing(RtcPeerConnection::new().map_err(fail)?);
        let _events = watch_peer(&a.0, &trace);
        let _channel = a.0.create_data_channel("thiscord-self-test");
        let sdp = offer(&a.0, &trace).await?;
        let remote = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
        remote.set_sdp(&sdp);
        JsFuture::from(b.0.set_remote_description(&remote))
            .await
            .map_err(|_| "Second peer rejected the offer")?;
        let answer: RtcSessionDescriptionInit = JsFuture::from(b.0.create_answer())
            .await
            .map_err(|_| "Second peer cannot answer")?
            .unchecked_into();
        JsFuture::from(b.0.set_local_description(&answer))
            .await
            .map_err(|_| "Second peer cannot answer")?;
        for _ in 0..100 {
            if b.0.ice_gathering_state() == RtcIceGatheringState::Complete {
                break;
            }
            gloo_timers::future::TimeoutFuture::new(50).await;
        }
        let answer =
            b.0.local_description()
                .ok_or("Second peer has no answer")?
                .sdp();
        accept(&a.0, &trace, &answer).await?;
        let connected = wait_connected(&a.0).await;
        trace.finish(
            if connected {
                Outcome::Connected
            } else {
                Outcome::TimedOut
            },
            None,
        );
        trace.push(Some(&a.0)).await;
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        trace.finish(Outcome::Error, Some(&error));
        trace.push(None).await;
    }
}
async fn desktop_self_test(interfaces: bool) {
    let trace = Trace::new(if interfaces {
        Kind::InterfacesSelfTest
    } else {
        Kind::LoopbackSelfTest
    });
    let result = async {
        let pc =
            Closing(RtcPeerConnection::new().map_err(|_| "Cannot create WebRTC peer".to_owned())?);
        let _events = watch_peer(&pc.0, &trace);
        let init = RtcRtpTransceiverInit::new();
        init.set_direction(RtcRtpTransceiverDirection::Recvonly);
        pc.0.add_transceiver_with_str_and_init("video", &init);
        let sdp = offer(&pc.0, &trace).await?;
        let answer: ViewAnswer = native(
            "screen_bridge_probe",
            json!({"offer":{"id":trace.id(),"interfaces":interfaces,"sdp":sdp}}),
        )
        .await?;
        accept(&pc.0, &trace, &answer.sdp).await?;
        let connected = wait_connected(&pc.0).await;
        trace.finish(
            if connected {
                Outcome::Connected
            } else {
                Outcome::TimedOut
            },
            None,
        );
        trace.push(Some(&pc.0)).await;
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        trace.finish(Outcome::Error, Some(&error));
        trace.push(None).await;
    }
}

/// Whitelisted bridge scalars from browser stats, in the native key scheme.
async fn bridge_stats(pc: &RtcPeerConnection) -> BTreeMap<String, String> {
    let future = JsFuture::from(pc.get_stats());
    let result = futures_util::future::select(
        future,
        Box::pin(gloo_timers::future::TimeoutFuture::new(200)),
    )
    .await;
    let futures_util::future::Either::Left((Ok(report), _)) = result else {
        return BTreeMap::from([("transport.stats".into(), "unavailable".into())]);
    };
    let mut entries = Vec::new();
    if let Ok(Some(iterator)) = js_sys::try_iter(&report) {
        for entry in iterator.flatten() {
            entries.push(js_sys::Array::from(&entry).get(1));
        }
    }
    let count =
        |entry: &JsValue, name: &str| property_value(entry, name).as_f64().unwrap_or(0.0) as u64;
    let mut facts = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    let mut summary = [BTreeMap::<String, usize>::new(), BTreeMap::new()];
    for entry in &entries {
        let remote = match property(entry, "type").as_str() {
            "local-candidate" => false,
            "remote-candidate" => true,
            _ => continue,
        };
        let address = Some(property(entry, "address"))
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| property(entry, "ip"));
        let mut description = format!(
            "{} {} {}",
            property(entry, "protocol"),
            property(entry, "candidateType"),
            bridge::address_class(&address)
        );
        let network = property(entry, "networkType");
        if !network.is_empty() {
            description.push_str(&format!(" {network}"));
        }
        *summary[usize::from(remote)]
            .entry(description.clone())
            .or_default() += 1;
        candidates.insert(property(entry, "id"), description);
    }
    let mut pairs = Vec::new();
    for entry in &entries {
        match property(entry, "type").as_str() {
            "candidate-pair" => {
                let side = |name| {
                    candidates
                        .get(&property(entry, name))
                        .cloned()
                        .unwrap_or_else(|| "unknown".into())
                };
                pairs.push(Pair {
                    state: property(entry, "state"),
                    nominated: property_value(entry, "nominated")
                        .as_bool()
                        .unwrap_or(false),
                    local: side("localCandidateId"),
                    remote: side("remoteCandidateId"),
                    requests_sent: count(entry, "requestsSent"),
                    requests_received: count(entry, "requestsReceived"),
                    responses_sent: count(entry, "responsesSent"),
                    responses_received: count(entry, "responsesReceived"),
                    bytes_sent: count(entry, "bytesSent"),
                    bytes_received: count(entry, "bytesReceived"),
                });
            }
            "transport" => {
                for (key, field) in [
                    ("transport.ice_state", "iceState"),
                    ("transport.dtls_state", "dtlsState"),
                    ("transport.dtls_role", "dtlsRole"),
                    ("transport.ice_role", "iceRole"),
                    (
                        "transport.selected_pair_changes",
                        "selectedCandidatePairChanges",
                    ),
                    ("transport.packets_sent", "packetsSent"),
                    ("transport.packets_received", "packetsReceived"),
                ] {
                    facts.insert(key.into(), property(entry, field));
                }
            }
            "inbound-rtp" if property(entry, "kind") == "video" => {
                for (key, field) in [
                    ("rtp.packets_received", "packetsReceived"),
                    ("rtp.packets_lost", "packetsLost"),
                    ("rtp.frames_received", "framesReceived"),
                    ("rtp.frames_decoded", "framesDecoded"),
                    ("rtp.key_frames_decoded", "keyFramesDecoded"),
                    ("rtp.frames_dropped", "framesDropped"),
                    ("rtp.nack_count", "nackCount"),
                    ("rtp.pli_count", "pliCount"),
                    ("rtp.decoder", "decoderImplementation"),
                    ("rtp.power_efficient_decoder", "powerEfficientDecoder"),
                ] {
                    facts.insert(key.into(), property(entry, field));
                }
            }
            _ => {}
        }
    }
    for (key, counts) in ["candidates.local", "candidates.remote"]
        .into_iter()
        .zip(summary)
    {
        let value = counts
            .into_iter()
            .map(|(k, n)| format!("{k}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        facts.insert(
            key.into(),
            if value.is_empty() {
                "none".into()
            } else {
                value
            },
        );
    }
    facts.extend(bridge::pair_facts(&pairs));
    facts
}

// Report rates from interval deltas, not selected/nominal quality.
pub fn describe(
    current: &thiscord_shared::screen::Diagnostics,
    previous: &thiscord_shared::screen::Diagnostics,
) -> String {
    let seconds = current.elapsed_ms.saturating_sub(previous.elapsed_ms) as f64 / 1000.0;
    let mut text = format!(
        "Pipeline sample: {:.3}s since start\n",
        current.elapsed_ms as f64 / 1000.0
    );
    for (name, value) in &current.labels {
        text.push_str(&format!("{name}: {value}\n"));
    }
    for (name, value) in &current.counters {
        let rate = if seconds > 0.0 {
            value.saturating_sub(*previous.counters.get(name).unwrap_or(&0)) as f64 / seconds
        } else {
            0.0
        };
        if name.starts_with("sfu_") || name.contains("peak") {
            text.push_str(&format!("{name}: {value}\n"));
        } else {
            text.push_str(&format!("{name}: {value} ({rate:.1}/s)\n"));
        }
    }
    for (name, value) in &current.timings {
        text.push_str(&format!(
            "{name}: mean {:.2} ms, max {:.2} ms, samples {}\n",
            value.total_us as f64 / value.count.max(1) as f64 / 1000.0,
            value.max_us as f64 / 1000.0,
            value.count
        ));
    }
    for (at, event) in &current.events {
        text.push_str(&format!("[{:.3}s] {event}\n", *at as f64 / 1000.0));
    }
    text
}
async fn browser_stats(pc: &RtcPeerConnection) -> (String, u64, Option<DecoderSample>) {
    // Whitelist scalars: SDP, candidate addresses and stream identifiers stay out.
    let future = JsFuture::from(pc.get_stats());
    let result = futures_util::future::select(
        future,
        Box::pin(gloo_timers::future::TimeoutFuture::new(100)),
    )
    .await;
    let futures_util::future::Either::Left((Ok(report), _)) = result else {
        return ("Browser stats unavailable".into(), 0, None);
    };
    let mut output = serde_json::Map::new();
    if let Ok(Some(entries)) = js_sys::try_iter(&report) {
        for entry in entries.flatten() {
            let entry = js_sys::Array::from(&entry).get(1);
            let get = |name: &str| {
                js_sys::Reflect::get(&entry, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
            };
            if get("type").as_string().as_deref() != Some("inbound-rtp")
                || get("kind").as_string().as_deref() != Some("video")
            {
                continue;
            }
            for name in [
                "framesPerSecond",
                "frameWidth",
                "frameHeight",
                "framesDecoded",
                "framesDropped",
                "framesReceived",
                "keyFramesDecoded",
                "freezeCount",
                "totalFreezesDuration",
                "packetsLost",
                "packetsReceived",
                "jitter",
                "jitterBufferDelay",
                "jitterBufferTargetDelay",
                "jitterBufferMinimumDelay",
                "jitterBufferEmittedCount",
                "totalDecodeTime",
                "nackCount",
                "pliCount",
                "decoderImplementation",
                "powerEfficientDecoder",
            ] {
                let value = get(name);
                if let Some(number) = value.as_f64() {
                    output.insert(name.into(), json!(number));
                } else if let Some(value) = value.as_bool() {
                    output.insert(name.into(), json!(value));
                } else if let Some(value) = value.as_string() {
                    output.insert(name.into(), json!(value));
                }
            }
        }
    }
    for receiver in pc.get_receivers().iter() {
        if let Ok(value) = js_sys::Reflect::get(&receiver, &JsValue::from_str("jitterBufferTarget"))
            && let Some(target) = value.as_f64()
        {
            output.insert("playoutTargetMs".into(), json!(target));
        }
    }
    let decoder = (|| {
        Some(DecoderSample {
            at_ms: js_sys::Date::now(),
            received: output.get("framesReceived")?.as_f64()? as u64,
            decoded: output.get("framesDecoded")?.as_f64()? as u64,
            decode_seconds: output.get("totalDecodeTime")?.as_f64()?,
        })
    })();
    let pli = output
        .get("pliCount")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;
    (
        format!(
            "Browser video (local hop only):\n{}",
            serde_json::to_string_pretty(&output).unwrap_or_default()
        ),
        pli,
        decoder,
    )
}
