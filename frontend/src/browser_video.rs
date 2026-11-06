//! Pure rules for browser screen sharing and cameras. The WASM client
//! (`browser_voice/video.rs`) owns the browser objects; codec selection, SDP
//! restriction, capture constraints, encoder limits, viewer feedback and
//! keyframe timing stay target-independent so they are unit tested natively.
use thiscord_shared::{
    screen::{MAX_VIEW_BITRATE, MIN_VIEW_BITRATE, Quality},
    video_feedback::ReceiverSample,
    voice::MediaKind,
};

/// Same encoder ceiling as the desktop WebView camera.
pub const CAMERA_MAX_BITRATE: u32 = 1_500_000;
pub const CAMERA_WIDTH: u32 = 1280;
pub const CAMERA_HEIGHT: u32 = 720;
pub const CAMERA_FPS: u32 = 30;
/// Minimum gap between `ScreenViews` messages.
pub const VIEWS_MS: f64 = 500.0;
/// Unchanged views are renewed this often, inside the SFU's three-second lease.
pub const VIEWS_RENEW_MS: f64 = 1_000.0;
/// Changed views go out after `VIEWS_MS`; an unchanged non-empty list is only
/// renewed (each message takes the SFU room write lock); an unchanged empty
/// list, already sent once, is not repeated.
pub fn views_due(changed: bool, empty: bool, since_ms: f64) -> bool {
    since_ms >= VIEWS_MS && (changed || (!empty && since_ms >= VIEWS_RENEW_MS))
}
/// Viewer feedback and stall sampling, like the native receiver.
pub const FEEDBACK_MS: f64 = 1_000.0;
/// No decoded frame for this long while packets arrive requests an IDR.
pub const STALL_MS: f64 = 2_000.0;
/// Minimum spacing of one view's keyframe requests (the SFU also coalesces).
pub const KEYFRAME_SPACING_MS: f64 = 1_000.0;

/// Resolutions offered by the browser share picker, like the desktop picker.
pub const SCREEN_HEIGHTS: [u32; 4] = [720, 1080, 1440, 2160];
pub const SCREEN_FPS: [u32; 3] = [15, 30, 60];

/// Whether an RTP codec capability or SDP format is H.264 that every Thiscord
/// receiver decodes: (constrained) baseline with packetization mode 1. RTX,
/// RED and FEC are excluded: the SFU forwards one plain media stream.
pub fn usable_h264(mime: &str, fmtp: &str) -> bool {
    if !mime.eq_ignore_ascii_case("video/h264") {
        return false;
    }
    let mut packetization = false;
    let mut baseline = false;
    for parameter in fmtp.split(';') {
        let Some((key, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "packetization-mode" => packetization = value.trim() == "1",
            "profile-level-id" => {
                let value = value.trim();
                baseline = value.len() == 6
                    && value.is_ascii()
                    && value[..2].eq_ignore_ascii_case("42")
                    && u8::from_str_radix(&value[2..4], 16).is_ok()
                    && u8::from_str_radix(&value[4..], 16).is_ok();
            }
            _ => {}
        }
    }
    packetization && baseline
}

/// Constrained baseline (`42e0..`/`42c0..`) sorts before plain baseline, so
/// the browser's first (sending) choice is the profile every decoder takes.
pub fn h264_rank(fmtp: &str) -> u8 {
    let constrained = fmtp.split(';').any(|parameter| {
        parameter
            .trim()
            .split_once('=')
            .is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("profile-level-id")
                    && value
                        .trim()
                        .get(2..4)
                        .and_then(|flags| u8::from_str_radix(flags, 16).ok())
                        .is_some_and(|flags| flags & 0x40 != 0)
            })
    });
    u8::from(!constrained)
}

/// Removes every non-H.264-baseline payload type from the video sections of a
/// local answer. Used only where `setCodecPreferences` is unavailable. A video
/// section without a usable format is left unchanged; callers answer it
/// `inactive` (no decoder) or refuse to publish on it.
pub fn restrict_video(sdp: &str) -> String {
    let newline = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out: Vec<String> = Vec::new();
    let mut section: Vec<&str> = Vec::new();
    let flush = |section: &mut Vec<&str>, out: &mut Vec<String>| {
        let video = section.first().is_some_and(|l| l.starts_with("m=video "));
        let fields = section
            .first()
            .and_then(|l| l.strip_prefix("m="))
            .map(|m| m.split(' ').collect::<Vec<_>>())
            .unwrap_or_default();
        let format = |pt: &str| {
            let rtpmap = section.iter().find_map(|line| {
                line.strip_prefix("a=rtpmap:")
                    .and_then(|rest| rest.split_once(' '))
                    .filter(|(id, _)| *id == pt)
                    .map(|(_, codec)| codec.split('/').next().unwrap_or_default())
            });
            let fmtp = section
                .iter()
                .find_map(|line| {
                    line.strip_prefix("a=fmtp:")
                        .and_then(|rest| rest.split_once(' '))
                        .filter(|(id, _)| *id == pt)
                        .map(|(_, parameters)| parameters)
                })
                .unwrap_or_default();
            rtpmap.is_some_and(|codec| usable_h264(&format!("video/{codec}"), fmtp))
        };
        let kept: Vec<&str> = fields
            .iter()
            .skip(3)
            .copied()
            .filter(|pt| format(pt))
            .collect();
        let restrict = video && fields.len() > 3 && !kept.is_empty();
        for line in section.drain(..) {
            if !restrict {
                out.push(line.to_owned());
            } else if line.starts_with("m=") {
                out.push(format!("m={} {}", fields[..3].join(" "), kept.join(" ")));
            } else {
                let attribute_pt = ["a=rtpmap:", "a=fmtp:", "a=rtcp-fb:"]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                    .and_then(|rest| rest.split(' ').next());
                if attribute_pt.is_none_or(|pt| kept.contains(&pt)) {
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

/// `getDisplayMedia` video constraints for a picker preset. The browser may
/// deliver less; it never upscales above the source.
pub fn display_constraints(quality: Quality) -> serde_json::Value {
    serde_json::json!({
        "width": { "ideal": quality.width(), "max": quality.width() },
        "height": { "ideal": quality.height, "max": quality.height },
        "frameRate": { "ideal": quality.fps, "max": quality.fps },
    })
}

/// Which physical camera to request on phones and tablets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Facing {
    #[default]
    User,
    Environment,
}
impl Facing {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Environment => "environment",
        }
    }
    pub fn flipped(self) -> Self {
        match self {
            Self::User => Self::Environment,
            Self::Environment => Self::User,
        }
    }
}

/// `getUserMedia` video constraints: a chosen device (`exact`, retried without
/// it when gone), otherwise the requested facing mode.
pub fn camera_constraints(device: Option<&str>, facing: Option<Facing>) -> serde_json::Value {
    let mut value = serde_json::json!({
        "width": { "ideal": CAMERA_WIDTH, "max": CAMERA_WIDTH },
        "height": { "ideal": CAMERA_HEIGHT, "max": CAMERA_HEIGHT },
        "frameRate": { "ideal": CAMERA_FPS, "max": CAMERA_FPS },
    });
    if let Some(device) = device.filter(|d| !d.is_empty()) {
        value["deviceId"] = serde_json::json!({ "exact": device });
    } else if let Some(facing) = facing {
        value["facingMode"] = serde_json::json!({ "ideal": facing.as_str() });
    }
    value
}

/// Encoder hint: smooth motion at 60 fps, otherwise sharp text and detail.
pub fn content_hint(kind: MediaKind, fps: u32) -> &'static str {
    match kind {
        MediaKind::Camera => "motion",
        _ if fps >= 60 => "motion",
        _ => "detail",
    }
}

/// One sender encoding derived from the SFU's `ScreenTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderLimit {
    /// No current viewers: the browser stops encoding and sending.
    pub active: bool,
    pub max_bitrate: u32,
}
/// The SFU's lowest live viewer budget, capped by the selected preset (or the
/// camera ceiling). Zero means no viewers and pauses the encoding.
pub fn encoder_limit(target: u32, maximum: u32) -> EncoderLimit {
    let maximum = maximum.clamp(MIN_VIEW_BITRATE, MAX_VIEW_BITRATE);
    EncoderLimit {
        active: target > 0,
        max_bitrate: target.clamp(MIN_VIEW_BITRATE, maximum),
    }
}
pub fn maximum_bitrate(kind: MediaKind, quality: Quality) -> u32 {
    if kind == MediaKind::Camera {
        CAMERA_MAX_BITRATE
    } else {
        quality.bitrate()
    }
}

/// Inbound video counters of one received track (`inbound-rtp` stats).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Inbound {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub frames_received: u64,
    pub frames_decoded: u64,
    pub decode_seconds: f64,
}
/// Receiver evidence for one interval, with the native client's thresholds
/// (`native_screen.rs`): >1% loss (and more than two packets) or decoder
/// pressure is congestion; ten packets and 15% loss is severe.
pub fn receiver_sample(
    previous: Inbound,
    current: Inbound,
    decoder_pressure: bool,
) -> ReceiverSample {
    let lost = current.packets_lost.saturating_sub(previous.packets_lost);
    let packets = current
        .packets_received
        .saturating_sub(previous.packets_received)
        .saturating_add(lost)
        .max(1);
    ReceiverSample {
        congested: (lost > 2 && lost.saturating_mul(100) > packets) || decoder_pressure,
        severe: lost >= 10 && lost.saturating_mul(100) >= packets.saturating_mul(15),
        receiving: current.frames_decoded > previous.frames_decoded,
    }
}

/// When one view needs a fresh IDR: once when it starts, then while packets
/// keep arriving without any newly decoded frame for `STALL_MS`, at most once
/// per `KEYFRAME_SPACING_MS`. An idle (static) source is not a stall.
#[derive(Debug, Clone, Copy, Default)]
pub struct Recovery {
    started: bool,
    frames: u64,
    stalled_since: Option<f64>,
    last_request: Option<f64>,
}
impl Recovery {
    pub fn due(&mut self, current: Inbound, packets_arrived: bool, now_ms: f64) -> bool {
        let decoded = current.frames_decoded != self.frames;
        self.frames = current.frames_decoded;
        if decoded || !packets_arrived {
            self.stalled_since = None;
        } else if self.stalled_since.is_none() {
            self.stalled_since = Some(now_ms);
        }
        let stalled = self.stalled_since.is_some_and(|at| now_ms - at >= STALL_MS);
        let spaced = self
            .last_request
            .is_none_or(|at| now_ms - at >= KEYFRAME_SPACING_MS);
        let due = spaced && (!self.started || stalled);
        if due {
            self.started = true;
            self.last_request = Some(now_ms);
        }
        due
    }
}

/// Plain baseline H.264 capability on the receiving side of a browser decides
/// whether video m-lines are answered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoSupport {
    pub decode: bool,
    pub encode: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen_views_go_out_on_change_and_renew_inside_the_lease() {
        assert!(
            !views_due(true, false, VIEWS_MS - 1.0),
            "changes keep the minimum gap"
        );
        assert!(views_due(true, false, VIEWS_MS));
        assert!(
            views_due(true, true, VIEWS_MS),
            "an emptied list is sent once"
        );
        assert!(
            !views_due(false, true, 60_000.0),
            "an unchanged empty list is not repeated"
        );
        assert!(!views_due(false, false, VIEWS_RENEW_MS - 1.0));
        assert!(views_due(false, false, VIEWS_RENEW_MS));
        const {
            assert!(
                VIEWS_RENEW_MS + 100.0 < thiscord_shared::screen::VIEW_LEASE_SECS as f64 * 1_000.0
            )
        };
    }

    #[test]
    fn only_baseline_packetization_mode_1_h264_is_usable() {
        for fmtp in [
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "profile-level-id=42e034;packetization-mode=1",
            "packetization-mode=1;profile-level-id=42001f",
            "packetization-mode=1;profile-level-id=42C01F",
        ] {
            assert!(usable_h264("video/H264", fmtp), "{fmtp}");
        }
        for (mime, fmtp) in [
            (
                "video/H264",
                "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f",
            ),
            ("video/H264", "packetization-mode=1;profile-level-id=640c1f"),
            ("video/H264", "packetization-mode=1;profile-level-id=4d001f"),
            ("video/H264", "packetization-mode=1"),
            ("video/H264", "packetization-mode=1;profile-level-id=42e0zz"),
            ("video/VP8", ""),
            ("video/rtx", "apt=125"),
            ("video/red", ""),
            ("video/ulpfec", ""),
        ] {
            assert!(!usable_h264(mime, fmtp), "{mime} {fmtp}");
        }
        assert!(
            h264_rank("packetization-mode=1;profile-level-id=42e01f")
                < h264_rank("packetization-mode=1;profile-level-id=42001f")
        );
    }

    fn answer() -> String {
        [
            "v=0",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111 63",
            "a=mid:0",
            "a=rtpmap:111 opus/48000/2",
            "a=rtpmap:63 red/48000/2",
            "m=video 9 UDP/TLS/RTP/SAVPF 96 97 125 126 102",
            "a=mid:1",
            "a=rtpmap:96 VP8/90000",
            "a=rtcp-fb:96 nack",
            "a=rtpmap:97 rtx/90000",
            "a=fmtp:97 apt=96",
            "a=rtpmap:125 H264/90000",
            "a=rtcp-fb:125 nack pli",
            "a=fmtp:125 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "a=rtpmap:126 rtx/90000",
            "a=fmtp:126 apt=125",
            "a=rtpmap:102 H264/90000",
            "a=fmtp:102 packetization-mode=0;profile-level-id=42001f",
            "a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid",
            "m=video 9 UDP/TLS/RTP/SAVPF 96",
            "a=mid:2",
            "a=rtpmap:96 VP8/90000",
            "",
        ]
        .join("\r\n")
    }

    #[test]
    fn answer_restriction_keeps_only_baseline_h264_in_video_sections() {
        let restricted = restrict_video(&answer());
        assert!(restricted.contains("m=video 9 UDP/TLS/RTP/SAVPF 125\r\n"));
        assert!(restricted.contains("a=rtcp-fb:125 nack pli"));
        assert!(restricted.contains("a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid"));
        for removed in [
            "a=rtpmap:97 ",
            "a=fmtp:126 ",
            "a=rtpmap:102 ",
            "a=rtcp-fb:96 nack",
        ] {
            assert!(!restricted.contains(removed), "{removed}");
        }
        // Audio is untouched; a section without H.264 stays as it was.
        assert!(restricted.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111 63\r\n"));
        assert!(restricted.contains("a=mid:2\r\na=rtpmap:96 VP8/90000"));
        assert_eq!(restrict_video(&restricted), restricted);
    }

    #[test]
    fn capture_constraints_follow_presets_devices_and_facing() {
        let value = display_constraints(Quality {
            height: 1440,
            fps: 60,
        });
        assert_eq!(value["height"]["max"], 1440);
        assert_eq!(value["width"]["max"], 2560);
        assert_eq!(value["frameRate"]["ideal"], 60);
        let camera = camera_constraints(Some("device-1"), Some(Facing::Environment));
        assert_eq!(camera["deviceId"]["exact"], "device-1");
        assert!(camera.get("facingMode").is_none());
        let rear = camera_constraints(None, Some(Facing::User.flipped()));
        assert_eq!(rear["facingMode"]["ideal"], "environment");
        assert_eq!(rear["width"]["max"], CAMERA_WIDTH);
        assert!(camera_constraints(Some(""), None).get("deviceId").is_none());
        assert_eq!(content_hint(MediaKind::ScreenVideo, 30), "detail");
        assert_eq!(content_hint(MediaKind::ScreenVideo, 60), "motion");
        assert_eq!(content_hint(MediaKind::Camera, 15), "motion");
    }

    #[test]
    fn screen_targets_cap_bitrate_and_zero_pauses() {
        let maximum = maximum_bitrate(MediaKind::ScreenVideo, Quality::default());
        assert_eq!(maximum, Quality::default().bitrate());
        assert_eq!(
            encoder_limit(0, maximum),
            EncoderLimit {
                active: false,
                max_bitrate: MIN_VIEW_BITRATE
            }
        );
        assert_eq!(
            encoder_limit(MAX_VIEW_BITRATE, maximum).max_bitrate,
            maximum
        );
        assert_eq!(encoder_limit(900_000, maximum).max_bitrate, 900_000);
        assert_eq!(encoder_limit(1, maximum).max_bitrate, MIN_VIEW_BITRATE);
        let camera = maximum_bitrate(MediaKind::Camera, Quality::default());
        assert_eq!(
            encoder_limit(MAX_VIEW_BITRATE, camera).max_bitrate,
            CAMERA_MAX_BITRATE
        );
    }

    #[test]
    fn viewer_samples_match_native_thresholds() {
        let base = Inbound {
            packets_received: 1000,
            packets_lost: 0,
            frames_decoded: 100,
            ..Default::default()
        };
        let clean = Inbound {
            packets_received: 1300,
            frames_decoded: 130,
            ..base
        };
        let sample = receiver_sample(base, clean, false);
        assert!(!sample.congested && !sample.severe && sample.receiving);
        assert!(receiver_sample(base, clean, true).congested);
        let lossy = Inbound {
            packets_lost: 5,
            ..clean
        };
        let sample = receiver_sample(base, lossy, false);
        assert!(sample.congested && !sample.severe);
        let severe = Inbound {
            packets_received: 1050,
            packets_lost: 20,
            frames_decoded: 100,
            ..base
        };
        let sample = receiver_sample(base, severe, false);
        assert!(sample.severe && !sample.receiving);
        // Counter resets never underflow.
        assert!(!receiver_sample(clean, base, false).congested);
    }

    #[test]
    fn keyframes_are_requested_on_start_and_bounded_stalls() {
        let mut recovery = Recovery::default();
        let mut stats = Inbound::default();
        assert!(
            recovery.due(stats, false, 0.0),
            "first view requests an IDR"
        );
        assert!(!recovery.due(stats, true, 500.0));
        assert!(!recovery.due(stats, true, 2_000.0));
        assert!(
            recovery.due(stats, true, 2_500.0),
            "stalled while receiving"
        );
        assert!(!recovery.due(stats, true, 3_000.0), "spaced");
        assert!(recovery.due(stats, true, 3_500.0));
        stats.frames_decoded = 10;
        assert!(!recovery.due(stats, true, 4_000.0), "decoding again");
        assert!(
            !recovery.due(stats, false, 9_000.0),
            "idle source is not a stall"
        );
        assert!(
            !recovery.due(stats, true, 9_500.0),
            "resumed packets get time"
        );
        stats.frames_decoded = 11;
        assert!(!recovery.due(stats, true, 12_000.0));
    }
}
