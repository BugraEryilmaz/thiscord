//! Pure rules for browser voice. The WASM client (`browser_voice`) owns the
//! browser objects; these decisions stay target-independent so they are unit
//! tested natively: SFU m-line mapping, transceiver directions, transmit and
//! speaking gates, capture constraints and the reconnect policy. They mirror
//! the native client's protocol decisions without its RED/DSP machinery.
use thiscord_shared::{
    ApiError, ErrorCode,
    audio::{AudioSettings, SpeakerVolumeTarget, TransmitMode},
    voice::{MediaKind, REPLACED_CLOSE_CODE, ROOM_CAPACITY},
};

/// Remote indicators light at this decoded level (WebRTC `audioLevel`, 0..1).
pub const REMOTE_SPEAKING_LEVEL: f32 = 0.02;
/// Matches the native remote indicator hangover in docs/audio.md.
pub const SPEAKING_HANGOVER_MS: f64 = 250.0;
/// Keeps the voice-activation gate open across short pauses between words.
pub const ACTIVATION_HOLD_MS: f64 = 500.0;
/// Mixer slots (microphones, then screen audio) with one playback element each.
pub const PLAYBACK_SLOTS: usize = ROOM_CAPACITY * 2;
/// Stored per-speaker volumes are bounded; the oldest entries are dropped first.
pub const MAX_STORED_VOLUMES: usize = 512;

/// Why the microphone could not be opened. The call continues as a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneProblem {
    Insecure,
    Denied,
    NotFound,
    Busy,
    Other,
}
impl MicrophoneProblem {
    pub const ALL: [Self; 5] = [
        Self::Insecure,
        Self::Denied,
        Self::NotFound,
        Self::Busy,
        Self::Other,
    ];
    pub fn notice(self) -> &'static str {
        match self {
            Self::Insecure => {
                "Voice needs a secure (HTTPS) page to use your microphone. You joined as a listener."
            }
            Self::Denied => {
                "Microphone access is blocked. You joined as a listener. Allow the microphone in your browser’s site settings, then rejoin."
            }
            Self::NotFound => "No microphone found. You joined as a listener.",
            Self::Busy => "The microphone is busy or unavailable. You joined as a listener.",
            Self::Other => "Cannot open the microphone. You joined as a listener.",
        }
    }
}

/// One SDP media section, in offer order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub audio: bool,
    pub mid: Option<String>,
}

/// Media sections of an SDP blob in m-line order. Session-level attributes
/// before the first `m=` line are ignored.
pub fn sections(sdp: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for line in sdp.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("m=") {
            out.push(Section {
                audio: rest.starts_with("audio "),
                mid: None,
            });
        } else if let Some(mid) = line.strip_prefix("a=mid:")
            && let Some(last) = out.last_mut()
        {
            last.mid.get_or_insert_with(|| mid.trim().to_owned());
        }
    }
    out
}

/// The SFU offers exactly `MediaKind::TRACK_COUNT` m-lines in track order, each
/// with a unique mid and the media type of its kind. Anything else is refused.
pub fn valid_offer(sections: &[Section]) -> bool {
    sections.len() == MediaKind::TRACK_COUNT
        && sections.iter().enumerate().all(|(index, section)| {
            section.mid.as_deref().is_some_and(|mid| {
                !mid.is_empty()
                    && sections
                        .iter()
                        .filter(|other| other.mid.as_deref() == Some(mid))
                        .count()
                        == 1
            }) && MediaKind::from_track(index)
                .is_some_and(|(kind, _)| section.audio == !kind.is_video())
        })
}

/// Maps a negotiated transceiver's mid to the SFU's `(kind, slot)` by m-line
/// position, never by browser transceiver order.
pub fn track_for_mid(sections: &[Section], mid: &str) -> Option<(MediaKind, usize)> {
    if !valid_offer(sections) {
        return None;
    }
    let index = sections
        .iter()
        .position(|section| section.mid.as_deref() == Some(mid))?;
    MediaKind::from_track(index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    SendRecv,
    RecvOnly,
    Inactive,
}
impl Direction {
    /// `RTCRtpTransceiverDirection` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendRecv => "sendrecv",
            Self::RecvOnly => "recvonly",
            Self::Inactive => "inactive",
        }
    }
}

/// What this browser can do on the SFU's m-lines in its answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Answering {
    /// Speak permission and an open microphone.
    pub microphone: bool,
    /// Speak permission: screen, shared audio and camera may be published
    /// later without renegotiation, even by a listener without a microphone.
    pub media: bool,
    /// Baseline H.264 encoding (publishing video) and decoding (viewing).
    pub h264_send: bool,
    pub h264_receive: bool,
}

/// Answer direction for one SFU m-line. The browser sends its microphone,
/// shared screen audio, screen video and camera on its own slot's m-lines
/// (attached later with `replaceTrack`; an idle sender sends nothing), and
/// receives other members' audio and, with an H.264 decoder, their video.
pub fn direction(kind: MediaKind, slot: usize, own_slot: usize, answering: Answering) -> Direction {
    let send = |allowed: bool| {
        if allowed {
            Direction::SendRecv
        } else {
            Direction::Inactive
        }
    };
    match kind {
        MediaKind::Microphone if slot == own_slot => send(answering.microphone),
        MediaKind::SystemAudio if slot == own_slot => send(answering.media),
        MediaKind::ScreenVideo | MediaKind::Camera if slot == own_slot => {
            send(answering.media && answering.h264_send)
        }
        MediaKind::Microphone | MediaKind::SystemAudio => Direction::RecvOnly,
        MediaKind::ScreenVideo | MediaKind::Camera if answering.h264_receive => Direction::RecvOnly,
        MediaKind::ScreenVideo | MediaKind::Camera => Direction::Inactive,
    }
}

/// Playback (mixer) slot for a received track, or `None` for video and for
/// this member's own media.
pub fn playback_slot(kind: MediaKind, slot: usize, own_slot: usize) -> Option<usize> {
    (slot != own_slot && slot < ROOM_CAPACITY)
        .then(|| kind.mixer_slot(slot))
        .flatten()
}

/// Removes RED (RFC 2198) payload types from every audio section of a local
/// answer. Only used where `setCodecPreferences` is unavailable: the SFU
/// accepts a plain Opus microphone from browser publishers, never RED.
pub fn strip_red(sdp: &str) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out: Vec<String> = Vec::new();
    let mut section: Vec<&str> = Vec::new();
    let flush = |section: &mut Vec<&str>, out: &mut Vec<String>| {
        let red: Vec<&str> = section
            .iter()
            .filter_map(|line| line.strip_prefix("a=rtpmap:"))
            .filter_map(|rest| rest.split_once(' '))
            .filter(|(_, codec)| codec.to_ascii_lowercase().starts_with("red/"))
            .map(|(pt, _)| pt)
            .collect();
        let audio = section.first().is_some_and(|l| l.starts_with("m=audio "));
        let fields = section
            .first()
            .and_then(|l| l.strip_prefix("m="))
            .map(|m| m.split(' ').collect::<Vec<_>>())
            .unwrap_or_default();
        // Never empty a section: keep it unchanged if RED is all it offers.
        let strip = audio && !red.is_empty() && fields.iter().skip(3).any(|pt| !red.contains(pt));
        for line in section.drain(..) {
            if !strip {
                out.push(line.to_owned());
            } else if line.starts_with("m=") {
                let kept: Vec<&str> = fields
                    .iter()
                    .skip(3)
                    .copied()
                    .filter(|pt| !red.contains(pt))
                    .collect();
                out.push(format!("m={} {}", fields[..3].join(" "), kept.join(" ")));
            } else {
                let attribute_pt = ["a=rtpmap:", "a=fmtp:", "a=rtcp-fb:"]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                    .and_then(|rest| rest.split(' ').next());
                if !attribute_pt.is_some_and(|pt| red.contains(&pt)) {
                    out.push(line.to_owned());
                }
            }
        }
    };
    for line in sdp.split(newline) {
        if line.starts_with("m=") {
            flush(&mut section, &mut out);
        }
        section.push(line);
    }
    flush(&mut section, &mut out);
    out.join(newline)
}

/// Level hangover for one indicator or gate.
#[derive(Debug, Default, Clone, Copy)]
pub struct Activity {
    last: Option<f64>,
}
impl Activity {
    pub fn observe(&mut self, level: f32, threshold: f32, hold_ms: f64, now_ms: f64) -> bool {
        if level.is_finite() && level >= threshold {
            self.last = Some(now_ms);
        }
        self.active(hold_ms, now_ms)
    }
    pub fn active(&self, hold_ms: f64, now_ms: f64) -> bool {
        self.last
            .is_some_and(|at| now_ms >= at && now_ms - at <= hold_ms)
    }
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Inputs to the microphone send gate. Server restrictions override every
/// local control; deafen also stops sending, like the native client.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gate {
    pub can_speak: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub pressed: bool,
    /// Voice activation is open, or cannot be measured (fails open).
    pub voice_open: bool,
}
impl Gate {
    pub fn sending(self, settings: &AudioSettings) -> bool {
        let allowed = self.can_speak
            && !self.server_muted
            && !self.server_deafened
            && !settings.muted
            && !settings.deafened;
        allowed
            && match settings.mode {
                TransmitMode::PushToTalk => self.pressed,
                TransmitMode::VoiceActivity => self.voice_open,
            }
    }
}

/// `HTMLMediaElement.volume` for one stream. Browsers cap element volume at
/// 100%, so larger gains play at full scale.
pub fn element_volume(stream_gain: f32, output_volume: f32, deafened: bool) -> f64 {
    if deafened {
        return 0.0;
    }
    let value = f64::from(stream_gain) * f64::from(output_volume);
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The getUserMedia request derived from stored settings. Only these fields
/// require reopening the microphone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub device: Option<String>,
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
    pub automatic_gain: bool,
}
impl Capture {
    pub fn from_settings(settings: &AudioSettings) -> Self {
        Self {
            device: settings.input.clone().filter(|id| !id.is_empty()),
            echo_cancellation: settings.echo_cancellation,
            noise_suppression: settings.noise_suppression,
            automatic_gain: settings.automatic_gain,
        }
    }
    /// `MediaTrackConstraints` JSON. `exact_device` is retried without the
    /// device when it no longer exists.
    pub fn constraints(&self, exact_device: bool) -> serde_json::Value {
        let mut value = serde_json::json!({
            "echoCancellation": self.echo_cancellation,
            "noiseSuppression": self.noise_suppression,
            "autoGainControl": self.automatic_gain,
            "channelCount": 1,
        });
        if exact_device && let Some(device) = &self.device {
            value["deviceId"] = serde_json::json!({ "exact": device });
        }
        value
    }
}

/// Browser defaults: the browser's own echo cancellation, noise suppression and
/// automatic gain are on; native-only options keep their shared defaults.
pub fn browser_defaults() -> AudioSettings {
    AudioSettings {
        echo_cancellation: true,
        noise_suppression: true,
        automatic_gain: true,
        ..AudioSettings::default()
    }
}

/// Stable localStorage key for one speaker-volume preference.
pub fn volume_key(target: &SpeakerVolumeTarget) -> String {
    format!(
        "{}:{}:{}",
        target.guild_id,
        target.account_id,
        u8::from(target.shared_audio)
    )
}

/// Why an attempt ended, with the native client's retry classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub retryable: bool,
    pub kind: FailureKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Transport,
    Server,
    Final,
    Replaced,
    ResumeUnavailable,
}
impl Failure {
    pub fn temporary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            kind: FailureKind::Transport,
        }
    }
    pub fn final_(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            kind: FailureKind::Final,
        }
    }
    /// A displaced connection must never automatically take the call back.
    pub fn replaced() -> Self {
        Self {
            message: "This voice call was opened in another connection.".into(),
            retryable: false,
            kind: FailureKind::Replaced,
        }
    }
    pub fn revoked() -> Self {
        Self::final_("Voice access changed. Join again if you still have permission.")
    }
    /// The media session is gone; only an explicit new join is allowed.
    pub fn resume_unavailable() -> Self {
        Self {
            message: "Voice session ended. Join again to reconnect.".into(),
            retryable: false,
            kind: FailureKind::ResumeUnavailable,
        }
    }
    pub fn server(error: &ApiError) -> Self {
        Self {
            message: error.message.clone(),
            retryable: matches!(
                error.code,
                ErrorCode::Conflict
                    | ErrorCode::RateLimited
                    | ErrorCode::ServiceUnavailable
                    | ErrorCode::InternalError
            ),
            kind: FailureKind::Server,
        }
    }
    /// A WebSocket close. Policy/protocol closes are final and their bodies are
    /// never displayed.
    pub fn closed(code: u16) -> Self {
        if code == REPLACED_CLOSE_CODE {
            Self::replaced()
        } else if matches!(code, 1000 | 1001 | 1005 | 1006 | 1011..=1014) {
            Self::temporary("Voice connection closed")
        } else {
            Self::final_("Voice connection rejected or protocol unsupported")
        }
    }
    /// Signaling loss that may reattach to the same media session instead of
    /// rejoining. Server, replacement and final failures never resume.
    pub fn resumable(&self, has_resume_token: bool, peer_failed: bool) -> bool {
        self.retryable && self.kind == FailureKind::Transport && has_resume_token && !peer_failed
    }
}

/// Same schedule as the native supervisor: 1, 2, 4 … 30 s plus up to 500 ms
/// jitter; a call that stayed connected for 30 s resets it.
#[derive(Debug, Default)]
pub struct Backoff {
    failures: u32,
}
pub const STABLE_CALL_MS: f64 = 30_000.0;
impl Backoff {
    pub fn next_ms(&mut self, stable: bool, jitter: u32) -> u32 {
        if stable {
            self.failures = 0;
        }
        let seconds = (1_u32 << self.failures.min(5)).min(30);
        self.failures = self.failures.saturating_add(1);
        seconds * 1000 + jitter % 501
    }
}

/// Which microphone track the browser sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRoute {
    /// Boost and limiter through WebAudio (`MediaStreamAudioDestinationNode`).
    Processed,
    /// A clone of the captured track, gated on its own, so meters keep reading
    /// the capture without the WebAudio hop (limiter look-ahead and the
    /// destination node's buffering) when there is nothing to boost.
    Direct,
    /// The captured track itself, while WebAudio is not running.
    Raw,
}
pub fn send_route(graph: bool, running: bool, input_gain: f32) -> SendRoute {
    if !(graph && running) {
        SendRoute::Raw
    } else if crate::browser_audio::microphone_gain(input_gain) > 1.0 {
        SendRoute::Processed
    } else {
        SendRoute::Direct
    }
}

/// Voice-activation level sampling. The analyser window (2048 samples, about
/// 43 ms) spans two samples, so short onsets cannot fall between reads.
pub const GATE_MS: f64 = 20.0;
/// `getStats` cadence. While WebAudio meters drive speaking rings, stats only
/// feed connection diagnostics, which the UI reads every 500 ms; otherwise
/// inbound `audioLevel` is the speaking fallback and needs a faster pass.
pub fn stats_interval_ms(meters_running: bool) -> f64 {
    if meters_running { 1_000.0 } else { 200.0 }
}

/// Delay before the next signaling-resume attempt (native recovery schedule).
pub fn resume_delay_ms(failures: u32, jitter: u8) -> u32 {
    (250_u32 << failures.min(2)) + u32::from(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> String {
        let mut sdp =
            String::from("v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\na=group:BUNDLE");
        for index in 0..MediaKind::TRACK_COUNT {
            sdp.push_str(&format!(" {index}"));
        }
        sdp.push_str("\r\n");
        for index in 0..MediaKind::TRACK_COUNT {
            let (kind, _) = MediaKind::from_track(index).unwrap();
            let media = if kind.is_video() { "video" } else { "audio" };
            sdp.push_str(&format!(
                "m={media} 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=mid:{index}\r\na=sendrecv\r\n"
            ));
        }
        sdp
    }

    #[test]
    fn sfu_offer_maps_mids_to_kinds_and_slots_by_m_line_order() {
        let sections = sections(&offer());
        assert!(valid_offer(&sections));
        for kind in MediaKind::ALL {
            for slot in 0..ROOM_CAPACITY {
                let mid = kind.track_index(slot).to_string();
                assert_eq!(track_for_mid(&sections, &mid), Some((kind, slot)));
            }
        }
        assert_eq!(track_for_mid(&sections, "missing"), None);
    }

    #[test]
    fn malformed_offers_fail_closed() {
        let mut short = sections(&offer());
        short.pop();
        assert!(!valid_offer(&short));
        assert_eq!(track_for_mid(&short, "0"), None);
        let mut swapped = sections(&offer());
        swapped[0].audio = false;
        assert!(!valid_offer(&swapped));
        let mut duplicate = sections(&offer());
        duplicate[1].mid = Some("0".into());
        assert!(!valid_offer(&duplicate));
        let mut missing = sections(&offer());
        missing[2].mid = None;
        assert!(!valid_offer(&missing));
    }

    #[test]
    fn mids_are_read_per_section_and_session_attributes_are_ignored() {
        let parsed =
            sections("v=0\na=mid:session\nm=audio 9 X 111\na=mid:a\na=mid:b\nm=video 9 X 125\n");
        assert_eq!(
            parsed,
            vec![
                Section {
                    audio: true,
                    mid: Some("a".into())
                },
                Section {
                    audio: false,
                    mid: None
                }
            ]
        );
    }

    #[test]
    fn red_is_removed_only_from_audio_sections() {
        let answer = "v=0\r\na=group:BUNDLE 0 1\r\nm=audio 9 UDP/TLS/RTP/SAVPF 63 111\r\na=mid:0\r\na=rtpmap:63 red/48000/2\r\na=fmtp:63 111/111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\na=rtcp-fb:111 transport-cc\r\nm=video 9 UDP/TLS/RTP/SAVPF 63\r\na=mid:1\r\na=rtpmap:63 RED/90000\r\n";
        let stripped = strip_red(answer);
        assert_eq!(
            stripped,
            "v=0\r\na=group:BUNDLE 0 1\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\na=rtcp-fb:111 transport-cc\r\nm=video 9 UDP/TLS/RTP/SAVPF 63\r\na=mid:1\r\na=rtpmap:63 RED/90000\r\n"
        );
        // Opus-only answers are unchanged, and a RED-only section is not emptied.
        assert_eq!(strip_red(&stripped), stripped);
        let only_red = "m=audio 9 X 63\na=rtpmap:63 red/48000/2\n";
        assert_eq!(strip_red(only_red), only_red);
    }

    #[test]
    fn directions_send_own_media_and_receive_others() {
        let own = 3;
        let full = Answering {
            microphone: true,
            media: true,
            h264_send: true,
            h264_receive: true,
        };
        let d = |kind, slot, answering| direction(kind, slot, own, answering);
        for kind in MediaKind::ALL {
            assert_eq!(d(kind, own, full), Direction::SendRecv, "{kind:?}");
            assert_eq!(d(kind, 0, full), Direction::RecvOnly, "{kind:?}");
        }
        // A listener without a microphone can still publish screen and camera.
        let listener = Answering {
            microphone: false,
            ..full
        };
        assert_eq!(d(MediaKind::Microphone, own, listener), Direction::Inactive);
        assert_eq!(d(MediaKind::Camera, own, listener), Direction::SendRecv);
        // Without Speak permission nothing of its own is negotiated for sending.
        let receive_only = Answering {
            microphone: false,
            media: false,
            ..full
        };
        for kind in MediaKind::ALL {
            assert_eq!(d(kind, own, receive_only), Direction::Inactive);
        }
        // No H.264 encoder: no own video; no decoder: other video declined.
        let no_h264 = Answering {
            h264_send: false,
            h264_receive: false,
            ..full
        };
        for kind in [MediaKind::ScreenVideo, MediaKind::Camera] {
            assert_eq!(d(kind, own, no_h264), Direction::Inactive);
            for slot in (0..ROOM_CAPACITY).filter(|s| *s != own) {
                assert_eq!(d(kind, slot, no_h264), Direction::Inactive);
                assert_eq!(playback_slot(kind, slot, own), None);
            }
        }
        assert_eq!(d(MediaKind::SystemAudio, own, no_h264), Direction::SendRecv);
        assert_eq!(d(MediaKind::SystemAudio, 1, no_h264), Direction::RecvOnly);
        assert_eq!(playback_slot(MediaKind::Microphone, own, own), None);
        assert_eq!(playback_slot(MediaKind::Microphone, 1, own), Some(1));
        assert_eq!(
            playback_slot(MediaKind::SystemAudio, 1, own),
            Some(ROOM_CAPACITY + 1)
        );
        assert_eq!(playback_slot(MediaKind::SystemAudio, own, own), None);
    }

    #[test]
    fn activity_hangover_holds_then_releases() {
        let mut activity = Activity::default();
        assert!(!activity.observe(0.0, 0.02, SPEAKING_HANGOVER_MS, 0.0));
        assert!(activity.observe(0.5, 0.02, SPEAKING_HANGOVER_MS, 100.0));
        assert!(activity.observe(0.0, 0.02, SPEAKING_HANGOVER_MS, 350.0));
        assert!(!activity.observe(0.0, 0.02, SPEAKING_HANGOVER_MS, 351.0));
        assert!(!activity.observe(f32::NAN, 0.0, SPEAKING_HANGOVER_MS, 400.0));
        activity.observe(1.0, 0.02, SPEAKING_HANGOVER_MS, 500.0);
        activity.reset();
        assert!(!activity.active(SPEAKING_HANGOVER_MS, 500.0));
        // A clock that moved backwards cannot extend activity.
        activity.observe(1.0, 0.02, SPEAKING_HANGOVER_MS, 1000.0);
        assert!(!activity.active(SPEAKING_HANGOVER_MS, 900.0));
    }

    #[test]
    fn server_restrictions_and_self_controls_override_the_send_gate() {
        let mut settings = browser_defaults();
        let open = Gate {
            can_speak: true,
            voice_open: true,
            ..Gate::default()
        };
        assert!(open.sending(&settings));
        for gate in [
            Gate {
                can_speak: false,
                ..open
            },
            Gate {
                server_muted: true,
                ..open
            },
            Gate {
                server_deafened: true,
                ..open
            },
            Gate {
                voice_open: false,
                ..open
            },
        ] {
            assert!(!gate.sending(&settings));
        }
        settings.deafened = true;
        assert!(!open.sending(&settings));
        settings.deafened = false;
        settings.muted = true;
        assert!(!open.sending(&settings));
        settings.muted = false;
        settings.mode = TransmitMode::PushToTalk;
        assert!(!open.sending(&settings));
        assert!(
            Gate {
                pressed: true,
                voice_open: false,
                ..open
            }
            .sending(&settings)
        );
    }

    #[test]
    fn element_volume_is_clamped_and_deafen_silences() {
        assert_eq!(element_volume(1.0, 1.0, false), 1.0);
        assert_eq!(element_volume(2.0, 1.0, false), 1.0);
        assert_eq!(element_volume(0.5, 0.5, false), 0.25);
        assert_eq!(element_volume(1.0, 1.0, true), 0.0);
        assert_eq!(element_volume(f32::NAN, 1.0, false), 0.0);
        assert_eq!(element_volume(-1.0, 1.0, false), 0.0);
    }

    #[test]
    fn capture_constraints_follow_processing_settings_and_device() {
        let mut settings = browser_defaults();
        let capture = Capture::from_settings(&settings);
        assert_eq!(
            capture.constraints(true),
            serde_json::json!({"echoCancellation":true,"noiseSuppression":true,"autoGainControl":true,"channelCount":1})
        );
        settings.input = Some("mic-2".into());
        settings.echo_cancellation = false;
        let changed = Capture::from_settings(&settings);
        assert_ne!(capture, changed);
        assert_eq!(changed.constraints(true)["deviceId"]["exact"], "mic-2");
        assert!(changed.constraints(false).get("deviceId").is_none());
        assert_eq!(changed.constraints(true)["echoCancellation"], false);
        // Output, volume and native-only options never reopen the microphone.
        settings.output = Some("speaker".into());
        settings.output_volume = 0.5;
        settings.neural_echo = true;
        settings.muted = true;
        assert_eq!(Capture::from_settings(&settings), changed);
        settings.input = Some(String::new());
        assert_eq!(Capture::from_settings(&settings).device, None);
    }

    #[test]
    fn reconnect_rules_mirror_the_native_client() {
        assert!(!Failure::replaced().retryable);
        assert!(!Failure::revoked().retryable);
        assert!(!Failure::resume_unavailable().retryable);
        assert_eq!(Failure::closed(REPLACED_CLOSE_CODE), Failure::replaced());
        for code in [1000, 1001, 1006, 1011, 1014] {
            assert!(Failure::closed(code).retryable, "{code}");
        }
        for code in [1002, 1003, 1008, 4000, 4002] {
            assert!(!Failure::closed(code).retryable, "{code}");
        }
        for (code, retryable) in [
            (ErrorCode::Unauthorized, false),
            (ErrorCode::Forbidden, false),
            (ErrorCode::NotFound, false),
            (ErrorCode::Conflict, true),
            (ErrorCode::RateLimited, true),
            (ErrorCode::ServiceUnavailable, true),
            (ErrorCode::InternalError, true),
        ] {
            let error = ApiError {
                code,
                message: "failure".into(),
                request_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
                fields: vec![],
            };
            assert_eq!(Failure::server(&error).retryable, retryable);
            assert!(!Failure::server(&error).resumable(true, false));
        }
        let lost = Failure::temporary("Voice connection closed");
        assert!(lost.resumable(true, false));
        assert!(!lost.resumable(false, false));
        assert!(!lost.resumable(true, true));
        assert!(!Failure::replaced().resumable(true, false));
    }

    #[test]
    fn backoff_is_bounded_and_resets_after_a_stable_call() {
        let mut backoff = Backoff::default();
        for expected in [1, 2, 4, 8, 16, 30, 30] {
            assert_eq!(backoff.next_ms(false, 0), expected * 1000);
        }
        assert_eq!(backoff.next_ms(true, 250), 1250);
        assert_eq!(backoff.next_ms(false, 1002), 2000);
        assert_eq!(resume_delay_ms(0, 0), 250);
        assert_eq!(resume_delay_ms(9, 255), 1255);
    }

    #[test]
    fn volume_keys_distinguish_voice_and_screen_audio() {
        let target = SpeakerVolumeTarget {
            guild_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            account_id: "00000000-0000-0000-0000-000000000002".parse().unwrap(),
            shared_audio: false,
        };
        let shared = SpeakerVolumeTarget {
            shared_audio: true,
            ..target
        };
        assert_ne!(volume_key(&target), volume_key(&shared));
        assert!(volume_key(&shared).ends_with(":1"));
    }

    #[test]
    fn unboosted_microphones_skip_the_webaudio_send_hop() {
        assert_eq!(send_route(true, true, 1.0), SendRoute::Direct);
        assert_eq!(send_route(true, true, 0.5), SendRoute::Direct);
        assert_eq!(send_route(true, true, f32::NAN), SendRoute::Direct);
        assert_eq!(send_route(true, true, 1.5), SendRoute::Processed);
        assert_eq!(send_route(true, false, 3.0), SendRoute::Raw);
        assert_eq!(send_route(false, true, 3.0), SendRoute::Raw);
    }

    #[test]
    fn stats_slow_down_only_while_meters_cover_speaking() {
        assert!(stats_interval_ms(true) >= 1_000.0);
        assert!(stats_interval_ms(false) <= 200.0);
        // Two gate samples per analyser window.
        const { assert!(GATE_MS * 2.0 <= 2048.0 / 48.0) };
    }
}
