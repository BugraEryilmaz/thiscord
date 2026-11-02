//! Browser media objects. Audio is captured, encoded, decoded, echo-cancelled
//! and played by the browser; WASM only routes tracks, sets element volumes
//! and reads levels. No PCM or Opus passes through Rust.
use js_sys::{Array, Function, Reflect};
use std::cell::RefCell;
use thiscord_frontend::browser_call::{Capture, MicrophoneProblem as MicError, PLAYBACK_SLOTS};
use thiscord_shared::{audio::AudioDevice, voice::IceServer};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AnalyserNode, AudioContext, AudioContextState, GainNode, HtmlAudioElement, MediaDeviceInfo,
    MediaDeviceKind, MediaStream, MediaStreamAudioSourceNode, MediaStreamConstraints,
    MediaStreamTrack, RtcConfiguration, RtcIceServer, RtcPeerConnection, RtcRtpTransceiver,
};

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

/// `HTMLMediaElement.setSinkId` (Chromium, Firefox; not Safari).
pub(super) fn output_selectable() -> bool {
    Reflect::get(&js_sys::global(), &"HTMLMediaElement".into())
        .and_then(|class| Reflect::get(&class, &"prototype".into()))
        .and_then(|prototype| Reflect::has(&prototype, &"setSinkId".into()))
        .unwrap_or(false)
}

/// One playback element per mixer slot, plus the context used for the local
/// microphone meter. Created inside the join gesture so mobile browsers allow
/// playback; they live for the whole call, across reconnect attempts.
pub(super) struct Output {
    context: Option<AudioContext>,
    elements: Vec<HtmlAudioElement>,
}
impl Output {
    pub fn new() -> Self {
        let context = AudioContext::new().ok();
        if let Some(context) = &context {
            settle(context.resume().ok().map(|p| Ok(p.into())));
        }
        let elements = (0..PLAYBACK_SLOTS)
            .filter_map(|_| HtmlAudioElement::new().ok())
            .inspect(|element| element.set_autoplay(true))
            .collect();
        Self { context, elements }
    }
    pub fn context(&self) -> Option<&AudioContext> {
        self.context.as_ref()
    }
    /// The meter only works while the context runs; callers fail open otherwise.
    pub fn metering(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|context| context.state() == AudioContextState::Running)
    }
    pub fn attach(&self, slot: usize, track: &MediaStreamTrack) {
        let Some(element) = self.elements.get(slot) else {
            return;
        };
        if let Ok(stream) = MediaStream::new_with_tracks(&Array::of1(track)) {
            element.set_src_object(Some(&stream));
            play(element);
        }
    }
    pub fn detach_all(&self) {
        for element in &self.elements {
            let _ = element.pause();
            element.set_src_object(None);
        }
    }
    pub fn set_volume(&self, slot: usize, volume: f64) {
        // Applied every tick; write only changes.
        if let Some(element) = self.elements.get(slot) {
            if element.volume() != volume {
                element.set_volume(volume);
            }
            if element.muted() != (volume <= 0.0) {
                element.set_muted(volume <= 0.0);
            }
        }
    }
    pub fn set_sink(&self, device: &str) {
        let args = Array::of1(&device.into());
        for element in &self.elements {
            settle(call_optional(element, "setSinkId", &args));
        }
    }
    /// Retries playback from a user gesture after autoplay was refused, and
    /// after a mobile browser suspended a background tab.
    pub fn unlock(&self) {
        if let Some(context) = &self.context {
            settle(context.resume().ok().map(|p| Ok(p.into())));
        }
        super::set_blocked(false);
        for element in &self.elements {
            if element.src_object().is_some() && element.paused() {
                play(element);
            }
        }
    }
    pub fn close(&self) {
        self.detach_all();
        if let Some(context) = &self.context {
            settle(context.close().ok().map(|p| Ok(p.into())));
        }
    }
}
fn play(element: &HtmlAudioElement) {
    let Ok(promise) = element.play() else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = JsFuture::from(promise).await
            && error_name(&error) == "NotAllowedError"
        {
            super::set_blocked(true);
        }
    });
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

struct Meter {
    source: MediaStreamAudioSourceNode,
    analyser: AnalyserNode,
    sink: GainNode,
    samples: RefCell<Vec<f32>>,
}
pub(super) struct Microphone {
    stream: MediaStream,
    pub track: MediaStreamTrack,
    meter: Option<Meter>,
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
        let Some(track) = stream
            .get_audio_tracks()
            .get(0)
            .dyn_into::<MediaStreamTrack>()
            .ok()
        else {
            stop_stream(&stream);
            return Err(MicError::NotFound);
        };
        let meter = context.and_then(|context| meter(context, &stream));
        Ok(Self {
            stream,
            track,
            meter,
        })
    }
    pub fn metered(&self) -> bool {
        self.meter.is_some()
    }
    /// Peak amplitude of the processed microphone signal, if metering works.
    pub fn level(&self) -> Option<f32> {
        let meter = self.meter.as_ref()?;
        let mut samples = meter.samples.borrow_mut();
        meter.analyser.get_float_time_domain_data(&mut samples);
        Some(
            samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
                .min(1.0),
        )
    }
    pub fn set_enabled(&self, enabled: bool) {
        if self.track.enabled() != enabled {
            self.track.set_enabled(enabled);
        }
    }
    pub fn stop(&self) {
        if let Some(meter) = &self.meter {
            let _ = meter.source.disconnect();
            let _ = meter.analyser.disconnect();
            let _ = meter.sink.disconnect();
        }
        stop_stream(&self.stream);
    }
}
fn meter(context: &AudioContext, stream: &MediaStream) -> Option<Meter> {
    let source = context.create_media_stream_source(stream).ok()?;
    let analyser = context.create_analyser().ok()?;
    analyser.set_fft_size(2048);
    // A silent sink keeps every engine pulling the analyser.
    let sink = context.create_gain().ok()?;
    sink.gain().set_value(0.0);
    source.connect_with_audio_node(&analyser).ok()?;
    analyser.connect_with_audio_node(&sink).ok()?;
    sink.connect_with_audio_node(&context.destination()).ok()?;
    Some(Meter {
        source,
        analyser,
        sink,
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
            if id.is_empty() || id.len() > 2048 {
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
pub(super) async fn inbound_levels(pc: &RtcPeerConnection) -> Vec<InboundLevel> {
    let Ok(report) = JsFuture::from(pc.get_stats()).await else {
        return vec![];
    };
    let Some(Ok(values)) = call_optional(&report, "values", &Array::new()) else {
        return vec![];
    };
    let Ok(Some(iterator)) = js_sys::try_iter(&values) else {
        return vec![];
    };
    let text = |stat: &JsValue, key: &str| {
        Reflect::get(stat, &key.into())
            .ok()
            .and_then(|value| value.as_string())
    };
    iterator
        .filter_map(Result::ok)
        .filter(|stat| {
            text(stat, "type").as_deref() == Some("inbound-rtp")
                && text(stat, "kind").as_deref() == Some("audio")
        })
        .filter_map(|stat| {
            let level = Reflect::get(&stat, &"audioLevel".into()).ok()?.as_f64()?;
            Some((
                text(&stat, "trackIdentifier"),
                text(&stat, "mid"),
                level as f32,
            ))
        })
        .take(PLAYBACK_SLOTS * 2)
        .collect()
}
