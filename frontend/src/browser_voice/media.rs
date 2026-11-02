//! Browser media objects. The browser captures the microphone (with its own
//! echo cancellation, noise suppression and automatic gain), encodes, decodes
//! and plays audio. WASM builds a WebAudio graph, routes tracks, sets gains
//! and reads levels; voice PCM and Opus never pass through Rust. Soundboard
//! clips and the test tone are the only samples WASM touches, outside any
//! audio callback.
//!
//! Playback graph (one `AudioContext` per call or device test):
//!
//! ```text
//! remote track ─ muted <audio> (Chrome pulls remote audio only while attached)
//!      └─ MediaStreamSource ─ slot gain (0–2) ─┐
//! soundboard clip ─ clip gain (0–2) ─ bus ─────┼─ master (0–2, 0 deafened)
//! test tone / microphone loopback ─────────────┘    └─ limiter ─ makeup
//!   ─ context destination (AudioContext.setSinkId), or
//!   ─ MediaStreamDestination ─ <audio>.setSinkId (Firefox fallback)
//! ```
//!
//! Send graph: microphone track ─ source ─ boost (1–10×) ─ limiter ─ makeup ─
//! MediaStreamDestination track (sent). Meters read the source (raw) and the
//! boost output, before the send gate, so a closed gate keeps measuring.
use js_sys::{Array, Function, Reflect};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use thiscord_frontend::{
    browser_audio::{self as rules, ConnectionStats, RemoteSource},
    browser_call::{Capture, MicrophoneProblem as MicError, PLAYBACK_SLOTS},
};
use thiscord_shared::{audio::AudioDevice, voice::IceServer};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AnalyserNode, AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextState, AudioNode,
    AudioScheduledSourceNode, DynamicsCompressorNode, GainNode, HtmlAudioElement, MediaDeviceInfo,
    MediaDeviceKind, MediaStream, MediaStreamAudioDestinationNode, MediaStreamAudioSourceNode,
    MediaStreamConstraints, MediaStreamTrack, RtcConfiguration, RtcIceServer, RtcPeerConnection,
    RtcRtpTransceiver,
};

/// Limiter settings shared by playback and send paths: transparent below
/// -2 dBFS, 20:1 above it.
const LIMIT_DB: f32 = -2.0;
const LIMIT_RATIO: f32 = 20.0;
/// Remote speaking meter window (about 21 ms at 48 kHz).
const METER_SAMPLES: usize = 1024;
/// A context that stays suspended this long is reported as blocked playback.
const BLOCKED_AFTER_MS: f64 = 1_500.0;

fn json(value: serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::UNDEFINED)
}
fn error_name(error: &JsValue) -> String {
    Reflect::get(error, &"name".into())
        .ok()
        .and_then(|name| name.as_string())
        .unwrap_or_default()
}
/// Calls an optional browser method, returning `None` where it is missing.
fn call_optional(target: &JsValue, method: &str, args: &Array) -> Option<Result<JsValue, JsValue>> {
    let function = Reflect::get(target, &method.into())
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    Some(Reflect::apply(&function, target, args))
}
/// Awaits a promise-returning optional call, ignoring absence and rejection.
fn settle(result: Option<Result<JsValue, JsValue>>) {
    if let Some(Ok(value)) = result
        && let Ok(promise) = value.dyn_into::<js_sys::Promise>()
    {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}
fn prototype_has(class: &str, method: &str) -> bool {
    Reflect::get(&js_sys::global(), &class.into())
        .and_then(|class| Reflect::get(&class, &"prototype".into()))
        .and_then(|prototype| Reflect::has(&prototype, &method.into()))
        .unwrap_or(false)
}
/// `AudioContext.setSinkId` (Chromium 110+).
fn context_sink() -> bool {
    prototype_has("AudioContext", "setSinkId")
}
/// `HTMLMediaElement.setSinkId` (Chromium, Firefox 116+; not Safari).
fn element_sink() -> bool {
    prototype_has("HTMLMediaElement", "setSinkId")
}
pub(super) fn output_selectable() -> bool {
    context_sink() || element_sink()
}

/// A limiter followed by a gain that cancels the compressor's automatic
/// makeup gain, so levels below the threshold pass unchanged.
fn limiter(context: &AudioContext) -> Option<(DynamicsCompressorNode, GainNode)> {
    let limiter = context.create_dynamics_compressor().ok()?;
    limiter.threshold().set_value(LIMIT_DB);
    limiter.knee().set_value(0.0);
    limiter.ratio().set_value(LIMIT_RATIO);
    limiter.attack().set_value(0.002);
    limiter.release().set_value(0.08);
    let makeup = context.create_gain().ok()?;
    // Web Audio: makeupGain = (1 / fullRangeGain)^0.6, where fullRangeGain is
    // the curve's gain at 0 dBFS: threshold × (1 - 1/ratio) dB.
    let full_range_db = LIMIT_DB * (1.0 - 1.0 / LIMIT_RATIO);
    makeup
        .gain()
        .set_value(10_f32.powf(0.6 * full_range_db / 20.0));
    limiter.connect_with_audio_node(&makeup).ok()?;
    Some((limiter, makeup))
}
fn set_param(param: &web_sys::AudioParam, value: f32) {
    // Applied every tick; write only changes.
    if (param.value() - value).abs() > f32::EPSILON {
        param.set_value(value);
    }
}

struct Slot {
    /// Keeps Chrome decoding the remote track; muted while the graph plays it.
    element: HtmlAudioElement,
    /// Pass-through level meter before the slot gain. Chrome reports no
    /// inbound `audioLevel` for tracks played only through WebAudio.
    meter: Option<AnalyserNode>,
    gain: Option<GainNode>,
    source: RefCell<Option<MediaStreamAudioSourceNode>>,
    track: RefCell<Option<String>>,
    /// Without a graph: the slot gain applied to the element volume.
    volume: Cell<f32>,
}
struct Route {
    destination: MediaStreamAudioDestinationNode,
    element: HtmlAudioElement,
}
struct Graph {
    context: AudioContext,
    master: GainNode,
    limiter: DynamicsCompressorNode,
    makeup: GainNode,
    soundboard: GainNode,
    route: RefCell<Option<Route>>,
}

/// Call playback: one slot per mixer slot (microphones, then system audio),
/// the soundboard bus and the master volume. Created inside the join gesture
/// so mobile browsers allow playback; lives for the whole call, across
/// reconnect attempts.
pub(super) struct Output {
    graph: Option<Graph>,
    slots: Vec<Slot>,
    master: Cell<f32>,
    /// An element's play() was refused by autoplay policy.
    refused: Rc<Cell<bool>>,
    suspended_since: Cell<Option<f64>>,
    resumed_at: Cell<f64>,
    clips: Rc<Cell<usize>>,
    samples: RefCell<Vec<f32>>,
}
impl Output {
    pub fn new() -> Self {
        let graph = AudioContext::new().ok().and_then(|context| {
            settle(context.resume().ok().map(|p| Ok(p.into())));
            let master = context.create_gain().ok()?;
            let (limiter, makeup) = limiter(&context)?;
            let soundboard = context.create_gain().ok()?;
            master.connect_with_audio_node(&limiter).ok()?;
            makeup
                .connect_with_audio_node(&context.destination())
                .ok()?;
            soundboard.connect_with_audio_node(&master).ok()?;
            Some(Graph {
                context,
                master,
                limiter,
                makeup,
                soundboard,
                route: RefCell::new(None),
            })
        });
        let slots = (0..PLAYBACK_SLOTS)
            .filter_map(|_| {
                let element = HtmlAudioElement::new().ok()?;
                element.set_autoplay(true);
                let nodes = graph.as_ref().and_then(|graph| {
                    let meter = graph.context.create_analyser().ok()?;
                    meter.set_fft_size(METER_SAMPLES as u32);
                    let gain = graph.context.create_gain().ok()?;
                    meter.connect_with_audio_node(&gain).ok()?;
                    gain.connect_with_audio_node(&graph.master).ok()?;
                    element.set_muted(true);
                    Some((meter, gain))
                });
                let (meter, gain) = nodes.unzip();
                Some(Slot {
                    element,
                    meter,
                    gain,
                    source: RefCell::new(None),
                    track: RefCell::new(None),
                    volume: Cell::new(1.0),
                })
            })
            .collect();
        Self {
            graph,
            slots,
            master: Cell::new(1.0),
            refused: Rc::default(),
            suspended_since: Cell::new(None),
            resumed_at: Cell::new(0.0),
            clips: Rc::default(),
            samples: RefCell::new(vec![0.0; METER_SAMPLES]),
        }
    }
    pub fn context(&self) -> Option<&AudioContext> {
        self.graph.as_ref().map(|graph| &graph.context)
    }
    /// The graph processes audio. Meters, boost and the processed microphone
    /// only work while it runs; callers fail open otherwise.
    pub fn running(&self) -> bool {
        self.graph
            .as_ref()
            .is_some_and(|graph| graph.context.state() == AudioContextState::Running)
    }

    /// Plays a remote track as `source` of the member in `member_slot`,
    /// replacing whatever that slot played. The slot's gain (set with
    /// [`Self::set_stream_gain`]) and the master volume, deafen and output
    /// device apply. Re-attaching the same track is a no-op.
    pub fn play_remote(&self, source: RemoteSource, member_slot: usize, track: &MediaStreamTrack) {
        if let Some(slot) = source.slot(member_slot) {
            self.attach(slot, track);
        }
    }
    /// Stops playing the member's `source` track.
    pub fn stop_remote(&self, source: RemoteSource, member_slot: usize) {
        if let Some(slot) = source.slot(member_slot).and_then(|s| self.slots.get(s)) {
            detach(slot);
        }
    }
    /// Linear playback gain (0–2) for one member's microphone or system audio.
    pub fn set_stream_gain(&self, source: RemoteSource, member_slot: usize, gain: f32) {
        let Some(slot) = source.slot(member_slot).and_then(|s| self.slots.get(s)) else {
            return;
        };
        let gain = rules::stream_gain(gain);
        match &slot.gain {
            Some(node) => set_param(&node.gain(), gain),
            None => {
                slot.volume.set(gain);
                self.apply_element_volume(slot);
            }
        }
    }
    fn attach(&self, slot: usize, track: &MediaStreamTrack) {
        let Some(slot) = self.slots.get(slot) else {
            return;
        };
        let id = track.id();
        if slot.track.borrow().as_deref() == Some(id.as_str()) {
            return;
        }
        detach(slot);
        let Ok(stream) = MediaStream::new_with_tracks(&Array::of1(track)) else {
            return;
        };
        slot.element.set_src_object(Some(&stream));
        play(&slot.element, &self.refused);
        if let (Some(graph), Some(meter)) = (&self.graph, &slot.meter)
            && let Ok(source) = graph.context.create_media_stream_source(&stream)
            && source.connect_with_audio_node(meter).is_ok()
        {
            *slot.source.borrow_mut() = Some(source);
        }
        *slot.track.borrow_mut() = Some(id);
    }
    /// Decoded peak of one mixer slot before local gain, while the graph
    /// runs; `None` means the caller should use `getStats` levels.
    pub fn level(&self, slot: usize) -> Option<f32> {
        if !self.running() {
            return None;
        }
        let slot = self.slots.get(slot)?;
        let meter = slot.meter.as_ref()?;
        slot.source.borrow().as_ref()?;
        let mut samples = self.samples.borrow_mut();
        meter.get_float_time_domain_data(&mut samples);
        Some(
            samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
                .min(1.0),
        )
    }
    pub fn detach_all(&self) {
        for slot in &self.slots {
            detach(slot);
        }
    }
    /// Output volume (0–2), silenced while deafened.
    pub fn set_master(&self, output_volume: f32, deafened: bool) {
        let gain = rules::master_gain(output_volume, deafened);
        match &self.graph {
            Some(graph) => set_param(&graph.master.gain(), gain),
            None => {
                self.master.set(gain);
                for slot in &self.slots {
                    self.apply_element_volume(slot);
                }
            }
        }
    }
    /// Without WebAudio, elements play directly and cannot exceed 100%.
    fn apply_element_volume(&self, slot: &Slot) {
        let volume = f64::from(slot.volume.get() * self.master.get()).clamp(0.0, 1.0);
        if slot.element.volume() != volume {
            slot.element.set_volume(volume);
        }
        if slot.element.muted() != (volume <= 0.0) {
            slot.element.set_muted(volume <= 0.0);
        }
    }
    /// Selects the output device ("" is the system default).
    pub fn set_sink(&self, device: &str) {
        let args = Array::of1(&device.into());
        let Some(graph) = &self.graph else {
            for slot in &self.slots {
                settle(call_optional(&slot.element, "setSinkId", &args));
            }
            return;
        };
        if context_sink() {
            settle(call_optional(&graph.context, "setSinkId", &args));
            return;
        }
        if !element_sink() {
            return;
        }
        // Firefox: play the graph's output through one element that can
        // select a device; the system default keeps the direct destination.
        let _ = graph.makeup.disconnect();
        let mut route = graph.route.borrow_mut();
        if device.is_empty() {
            if let Some(route) = route.take() {
                let _ = route.element.pause();
                route.element.set_src_object(None);
            }
            let _ = graph
                .makeup
                .connect_with_audio_node(&graph.context.destination());
            return;
        }
        if route.is_none() {
            *route = graph
                .context
                .create_media_stream_destination()
                .ok()
                .zip(HtmlAudioElement::new().ok())
                .map(|(destination, element)| {
                    element.set_src_object(Some(&destination.stream()));
                    Route {
                        destination,
                        element,
                    }
                });
        }
        match route.as_ref() {
            Some(route) => {
                let _ = graph.makeup.connect_with_audio_node(&route.destination);
                settle(call_optional(&route.element, "setSinkId", &args));
                play(&route.element, &self.refused);
            }
            None => {
                let _ = graph
                    .makeup
                    .connect_with_audio_node(&graph.context.destination());
            }
        }
    }
    /// Retries playback from a user gesture after autoplay was refused, and
    /// after a mobile browser suspended or interrupted a background tab.
    pub fn unlock(&self) {
        if let Some(graph) = &self.graph {
            settle(graph.context.resume().ok().map(|p| Ok(p.into())));
            if let Some(route) = graph.route.borrow().as_ref() {
                play(&route.element, &self.refused);
            }
        }
        self.refused.set(false);
        self.suspended_since.set(None);
        super::set_blocked(false);
        for slot in &self.slots {
            if slot.element.src_object().is_some() && slot.element.paused() {
                play(&slot.element, &self.refused);
            }
        }
    }
    /// Reports refused playback (a context that will not start, or an element
    /// autoplay refusal) and quietly retries resuming the context.
    pub fn check(&self, now_ms: f64) {
        let suspended = self.graph.as_ref().is_some_and(|graph| {
            let state = graph.context.state();
            state != AudioContextState::Running && state != AudioContextState::Closed
        });
        if !suspended {
            self.suspended_since.set(None);
        } else if self.suspended_since.get().is_none() {
            self.suspended_since.set(Some(now_ms));
        }
        if suspended
            && now_ms - self.resumed_at.get() > 1_000.0
            && let Some(graph) = &self.graph
        {
            // Allowed again after any earlier gesture (sticky activation).
            self.resumed_at.set(now_ms);
            settle(graph.context.resume().ok().map(|p| Ok(p.into())));
        }
        let blocked = self.refused.get()
            || self
                .suspended_since
                .get()
                .is_some_and(|since| now_ms - since > BLOCKED_AFTER_MS);
        super::set_blocked(blocked);
    }
    /// A mono buffer for a soundboard clip or the test tone.
    pub fn buffer(&self, samples: &[f32], sample_rate: f32) -> Option<AudioBuffer> {
        let graph = self.graph.as_ref()?;
        if samples.is_empty() {
            return None;
        }
        let buffer = graph
            .context
            .create_buffer(1, samples.len() as u32, sample_rate)
            .ok()?;
        buffer.copy_to_channel(samples, 0).ok()?;
        Some(buffer)
    }
    pub fn sample_rate(&self) -> Option<f32> {
        self.graph.as_ref().map(|graph| graph.context.sample_rate())
    }
    /// Plays a soundboard clip at a person's level through the master volume
    /// and output device. At most `SOUND_OVERLAP` clips overlap.
    pub fn play_clip(&self, buffer: &AudioBuffer, gain: f32) -> bool {
        let Some(graph) = &self.graph else {
            return false;
        };
        if self.clips.get() >= rules::SOUND_OVERLAP {
            return false;
        }
        let started = start(&graph.context, buffer, gain, &graph.soundboard);
        if started.is_some() {
            let clips = self.clips.clone();
            clips.set(clips.get() + 1);
            if let Some(source) = &started {
                on_ended(source, move || clips.set(clips.get().saturating_sub(1)));
            }
        }
        started.is_some()
    }
    /// Plays the test tone through master volume and the output device.
    pub fn play_test(&self, buffer: &AudioBuffer) -> Option<AudioBufferSourceNode> {
        let graph = self.graph.as_ref()?;
        start(&graph.context, buffer, 1.0, &graph.master)
    }
    /// The node that loopback audio (the microphone test) connects to.
    pub fn input(&self) -> Option<&AudioNode> {
        self.graph
            .as_ref()
            .map(|graph| graph.master.unchecked_ref())
    }
    pub fn close(&self) {
        self.detach_all();
        if let Some(graph) = &self.graph {
            if let Some(route) = graph.route.borrow_mut().take() {
                let _ = route.element.pause();
                route.element.set_src_object(None);
            }
            let _ = graph.limiter.disconnect();
            settle(graph.context.close().ok().map(|p| Ok(p.into())));
        }
    }
}
fn detach(slot: &Slot) {
    if let Some(source) = slot.source.borrow_mut().take() {
        let _ = source.disconnect();
    }
    slot.track.borrow_mut().take();
    let _ = slot.element.pause();
    slot.element.set_src_object(None);
}
fn play(element: &HtmlAudioElement, refused: &Rc<Cell<bool>>) {
    let Ok(promise) = element.play() else {
        return;
    };
    let refused = refused.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = JsFuture::from(promise).await
            && error_name(&error) == "NotAllowedError"
        {
            refused.set(true);
            super::set_blocked(true);
        }
    });
}
/// Starts a buffer through its own gain node into `into`; nodes disconnect
/// when it ends.
fn start(
    context: &AudioContext,
    buffer: &AudioBuffer,
    gain: f32,
    into: &AudioNode,
) -> Option<AudioBufferSourceNode> {
    let source = context.create_buffer_source().ok()?;
    let level = context.create_gain().ok()?;
    source.set_buffer(Some(buffer));
    level.gain().set_value(gain);
    if source.connect_with_audio_node(&level).is_err()
        || level.connect_with_audio_node(into).is_err()
        || source.start().is_err()
    {
        let _ = source.disconnect();
        let _ = level.disconnect();
        return None;
    }
    let cleanup = source.clone();
    on_ended(&source, move || {
        let _ = cleanup.disconnect();
        let _ = level.disconnect();
    });
    Some(source)
}
/// Chains `f` after any existing `onended` handler of the node.
fn on_ended(source: &AudioBufferSourceNode, f: impl FnOnce() + 'static) {
    let node: &AudioScheduledSourceNode = source.unchecked_ref();
    let previous = node.onended();
    let callback = Closure::once_into_js(move || {
        if let Some(previous) = previous {
            let _ = previous.call0(&JsValue::NULL);
        }
        f();
    });
    node.set_onended(Some(callback.unchecked_ref()));
}

/// getUserMedia failure categories; the browser's error text is never shown.
fn microphone_problem(error: &JsValue) -> MicError {
    match error_name(error).as_str() {
        "NotAllowedError" | "SecurityError" | "PermissionDeniedError" => MicError::Denied,
        "NotFoundError" | "OverconstrainedError" | "DevicesNotFoundError" => MicError::NotFound,
        "NotReadableError" | "AbortError" | "TrackStartError" => MicError::Busy,
        _ => MicError::Other,
    }
}

/// WebAudio processing of the captured microphone: boost, limiter and meters.
struct Send {
    source: MediaStreamAudioSourceNode,
    boost: GainNode,
    limiter: DynamicsCompressorNode,
    makeup: GainNode,
    destination: MediaStreamAudioDestinationNode,
    /// The processed track sent to the SFU while the graph runs.
    track: MediaStreamTrack,
    raw_meter: AnalyserNode,
    meter: AnalyserNode,
    sink: GainNode,
    loopback: RefCell<Option<GainNode>>,
    samples: RefCell<Vec<f32>>,
}
pub(super) struct Microphone {
    stream: MediaStream,
    /// The captured track: sent directly only while the graph cannot run.
    raw: MediaStreamTrack,
    send: Option<Send>,
}
impl Microphone {
    pub async fn open(capture: &Capture, context: Option<&AudioContext>) -> Result<Self, MicError> {
        let window = web_sys::window().ok_or(MicError::Other)?;
        if !window.is_secure_context() {
            return Err(MicError::Insecure);
        }
        let devices = window
            .navigator()
            .media_devices()
            .map_err(|_| MicError::Insecure)?;
        let request = |exact: bool| {
            let constraints = MediaStreamConstraints::new();
            constraints.set_audio(&json(capture.constraints(exact)));
            constraints.set_video(&JsValue::FALSE);
            devices.get_user_media_with_constraints(&constraints)
        };
        let mut result = match request(true) {
            Ok(promise) => JsFuture::from(promise).await,
            Err(error) => Err(error),
        };
        // A remembered device may be gone; fall back to the system default.
        if capture.device.is_some()
            && let Err(error) = &result
            && matches!(microphone_problem(error), MicError::NotFound)
        {
            result = match request(false) {
                Ok(promise) => JsFuture::from(promise).await,
                Err(error) => Err(error),
            };
        }
        let stream: MediaStream = result
            .map_err(|error| microphone_problem(&error))?
            .unchecked_into();
        let Some(raw) = stream
            .get_audio_tracks()
            .get(0)
            .dyn_into::<MediaStreamTrack>()
            .ok()
        else {
            stop_stream(&stream);
            return Err(MicError::NotFound);
        };
        let send = context.and_then(|context| send_graph(context, &stream));
        Ok(Self { stream, raw, send })
    }
    /// Whether meters and boost exist (they work while the context runs).
    pub fn metered(&self) -> bool {
        self.send.is_some()
    }
    /// The track to send: processed while the graph runs, else the raw track.
    pub fn track(&self, processed: bool) -> &MediaStreamTrack {
        match &self.send {
            Some(send) if processed => &send.track,
            _ => &self.raw,
        }
    }
    pub fn set_boost(&self, input_gain: f32) {
        if let Some(send) = &self.send {
            set_param(&send.boost.gain(), rules::microphone_gain(input_gain));
        }
    }
    /// The send gate. The processed route keeps the captured track enabled so
    /// meters keep measuring; the raw route gates the captured track itself.
    pub fn set_sending(&self, sending: bool, processed: bool) {
        let set = |track: &MediaStreamTrack, enabled: bool| {
            if track.enabled() != enabled {
                track.set_enabled(enabled);
            }
        };
        match &self.send {
            Some(send) if processed => {
                set(&self.raw, true);
                set(&send.track, sending);
            }
            Some(send) => {
                set(&send.track, false);
                set(&self.raw, sending);
            }
            None => set(&self.raw, sending),
        }
    }
    fn peak(&self, raw: bool) -> Option<f32> {
        let send = self.send.as_ref()?;
        let analyser = if raw { &send.raw_meter } else { &send.meter };
        let mut samples = send.samples.borrow_mut();
        analyser.get_float_time_domain_data(&mut samples);
        Some(
            samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
                .min(1.0),
        )
    }
    /// Peak after boost, before the send gate (what voice activation sees).
    pub fn level(&self) -> Option<f32> {
        self.peak(false)
    }
    /// Peak of the browser-processed capture before boost.
    pub fn raw_level(&self) -> Option<f32> {
        self.peak(true)
    }
    /// Plays the boosted microphone into `into` (device test loopback).
    pub fn loopback(&self, into: Option<&AudioNode>) {
        let Some(send) = &self.send else {
            return;
        };
        if let Some(previous) = send.loopback.borrow_mut().take() {
            let _ = previous.disconnect();
        }
        let Some(into) = into else {
            return;
        };
        if let Ok(gain) = send.makeup.context().create_gain()
            && send.makeup.connect_with_audio_node(&gain).is_ok()
            && gain.connect_with_audio_node(into).is_ok()
        {
            *send.loopback.borrow_mut() = Some(gain);
        }
    }
    pub fn stop(&self) {
        if let Some(send) = &self.send {
            for node in [
                send.source.unchecked_ref::<AudioNode>(),
                send.boost.unchecked_ref(),
                send.limiter.unchecked_ref(),
                send.makeup.unchecked_ref(),
                send.destination.unchecked_ref(),
                send.raw_meter.unchecked_ref(),
                send.meter.unchecked_ref(),
                send.sink.unchecked_ref(),
            ] {
                let _ = node.disconnect();
            }
            if let Some(loopback) = send.loopback.borrow_mut().take() {
                let _ = loopback.disconnect();
            }
            send.track.stop();
        }
        stop_stream(&self.stream);
    }
}
fn send_graph(context: &AudioContext, stream: &MediaStream) -> Option<Send> {
    let source = context.create_media_stream_source(stream).ok()?;
    let boost = context.create_gain().ok()?;
    let (limiter, makeup) = limiter(context)?;
    let destination = context.create_media_stream_destination().ok()?;
    let track = destination
        .stream()
        .get_audio_tracks()
        .get(0)
        .dyn_into::<MediaStreamTrack>()
        .ok()?;
    let analyser = || {
        let analyser = context.create_analyser().ok()?;
        analyser.set_fft_size(2048);
        Some(analyser)
    };
    let (raw_meter, meter) = (analyser()?, analyser()?);
    // A silent sink keeps every engine pulling the analysers.
    let sink = context.create_gain().ok()?;
    sink.gain().set_value(0.0);
    let connected = source.connect_with_audio_node(&boost).is_ok()
        && boost.connect_with_audio_node(&limiter).is_ok()
        && makeup.connect_with_audio_node(&destination).is_ok()
        && source.connect_with_audio_node(&raw_meter).is_ok()
        && boost.connect_with_audio_node(&meter).is_ok()
        && raw_meter.connect_with_audio_node(&sink).is_ok()
        && meter.connect_with_audio_node(&sink).is_ok()
        && sink.connect_with_audio_node(&context.destination()).is_ok();
    if !connected {
        for node in [
            source.unchecked_ref::<AudioNode>(),
            boost.unchecked_ref(),
            limiter.unchecked_ref(),
            makeup.unchecked_ref(),
            raw_meter.unchecked_ref(),
            meter.unchecked_ref(),
            sink.unchecked_ref(),
        ] {
            let _ = node.disconnect();
        }
        track.stop();
        return None;
    }
    Some(Send {
        source,
        boost,
        limiter,
        makeup,
        destination,
        track,
        raw_meter,
        meter,
        sink,
        loopback: RefCell::new(None),
        samples: RefCell::new(vec![0.0; 2048]),
    })
}
fn stop_stream(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        track.unchecked_into::<MediaStreamTrack>().stop();
    }
}

/// Microphones, and speakers where outputs can be selected. Labels are empty
/// until the site has microphone permission; numbered names stand in.
pub(super) async fn devices() -> Result<Vec<AudioDevice>, String> {
    let devices = web_sys::window()
        .and_then(|w| w.navigator().media_devices().ok())
        .ok_or("Audio devices need a secure (HTTPS) page")?;
    let list: Array = JsFuture::from(
        devices
            .enumerate_devices()
            .map_err(|_| "Cannot list audio devices")?,
    )
    .await
    .map_err(|_| "Cannot list audio devices")?
    .unchecked_into();
    let outputs = output_selectable();
    let (mut inputs, mut speakers) = (0, 0);
    Ok(list
        .iter()
        .filter_map(|device| device.dyn_into::<MediaDeviceInfo>().ok())
        .filter_map(|device| {
            let input = match device.kind() {
                MediaDeviceKind::Audioinput => true,
                MediaDeviceKind::Audiooutput if outputs => false,
                _ => return None,
            };
            let id = device.device_id();
            // "default"/"communications" duplicate a real device on Chromium.
            if id.is_empty() || id.len() > 2048 || (!input && id == "default") {
                return None;
            }
            let count = if input { &mut inputs } else { &mut speakers };
            *count += 1;
            let label = device.label();
            Some(AudioDevice {
                name: if label.trim().is_empty() {
                    let kind = if input { "Microphone" } else { "Speaker" };
                    format!("{kind} {count}")
                } else {
                    label
                },
                id,
                input,
            })
        })
        .collect())
}

/// The offer's STUN/TURN servers; credentials are never logged.
pub(super) fn peer(servers: &[IceServer]) -> Result<RtcPeerConnection, JsValue> {
    let list = Array::new();
    for server in servers {
        let urls = server
            .urls
            .iter()
            .filter(|url| {
                ["stun:", "stuns:", "turn:", "turns:"]
                    .iter()
                    .any(|scheme| url.starts_with(scheme))
            })
            .map(|url| JsValue::from_str(url))
            .collect::<Array>();
        if urls.length() == 0 {
            continue;
        }
        let ice = RtcIceServer::new();
        ice.set_urls(&urls);
        if !server.username.is_empty() {
            ice.set_username(&server.username);
            ice.set_credential(&server.credential);
        }
        list.push(&ice);
    }
    let configuration = RtcConfiguration::new();
    configuration.set_ice_servers(&list);
    RtcPeerConnection::new_with_configuration(&configuration)
}

/// Browser Opus capabilities without RED or comfort noise, so the answer
/// negotiates plain Opus in both directions (`packet_redundancy: false`).
pub(super) fn opus_codecs() -> Option<Array> {
    let class = Reflect::get(&js_sys::global(), &"RTCRtpReceiver".into()).ok()?;
    let capabilities =
        call_optional(&class, "getCapabilities", &Array::of1(&"audio".into()))?.ok()?;
    let codecs: Array = Reflect::get(&capabilities, &"codecs".into())
        .ok()?
        .dyn_into()
        .ok()?;
    let opus = codecs
        .iter()
        .filter(|codec| {
            Reflect::get(codec, &"mimeType".into())
                .ok()
                .and_then(|m| m.as_string())
                .is_some_and(|m| m.eq_ignore_ascii_case("audio/opus"))
        })
        .collect::<Array>();
    (opus.length() > 0).then_some(opus)
}
/// Whether the browser accepted Opus-only preferences for this transceiver.
pub(super) fn prefer_codecs(transceiver: &RtcRtpTransceiver, codecs: &Array) -> bool {
    matches!(
        call_optional(transceiver, "setCodecPreferences", &Array::of1(codecs)),
        Some(Ok(_))
    )
}
pub(super) fn set_direction(transceiver: &RtcRtpTransceiver, direction: &str) -> bool {
    Reflect::set(transceiver, &"direction".into(), &direction.into()).unwrap_or(false)
}

/// Decoded audio level of one received track: `(trackIdentifier, mid, level)`.
pub(super) type InboundLevel = (Option<String>, Option<String>, f32);
/// One `getStats()` pass: remote speaking levels and connection diagnostics.
pub(super) struct Stats {
    pub levels: Vec<InboundLevel>,
    pub connection: ConnectionStats,
}
pub(super) async fn stats(pc: &RtcPeerConnection) -> Stats {
    let mut stats = Stats {
        levels: vec![],
        connection: ConnectionStats::default(),
    };
    let Ok(report) = JsFuture::from(pc.get_stats()).await else {
        return stats;
    };
    let Some(Ok(values)) = call_optional(&report, "values", &Array::new()) else {
        return stats;
    };
    let Ok(Some(iterator)) = js_sys::try_iter(&values) else {
        return stats;
    };
    let text = |stat: &JsValue, key: &str| {
        Reflect::get(stat, &key.into())
            .ok()
            .and_then(|value| value.as_string())
    };
    let number = |stat: &JsValue, key: &str| {
        Reflect::get(stat, &key.into())
            .ok()
            .and_then(|value| value.as_f64())
    };
    let flag = |stat: &JsValue, key: &str| {
        Reflect::get(stat, &key.into())
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    // Reports are bounded by the transceiver count; cap the walk anyway.
    for stat in iterator.filter_map(Result::ok).take(512) {
        let audio = || text(&stat, "kind").as_deref() == Some("audio");
        match text(&stat, "type").as_deref() {
            Some("candidate-pair") => stats.connection.candidate_pair(
                text(&stat, "state").as_deref() == Some("succeeded"),
                flag(&stat, "nominated") || flag(&stat, "selected"),
                number(&stat, "currentRoundTripTime"),
            ),
            Some("inbound-rtp") if audio() => {
                stats.connection.inbound(
                    number(&stat, "jitter"),
                    number(&stat, "packetsReceived"),
                    number(&stat, "packetsLost"),
                );
                if let Some(level) = number(&stat, "audioLevel")
                    && stats.levels.len() < PLAYBACK_SLOTS * 2
                {
                    stats.levels.push((
                        text(&stat, "trackIdentifier"),
                        text(&stat, "mid"),
                        level as f32,
                    ));
                }
            }
            Some("remote-inbound-rtp") if audio() => stats.connection.remote_inbound(
                number(&stat, "roundTripTime"),
                number(&stat, "jitter"),
                number(&stat, "fractionLost"),
            ),
            _ => {}
        }
    }
    stats
}
