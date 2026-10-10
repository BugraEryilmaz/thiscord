//! Screen-sharing metadata only. Pixels travel on encrypted WebRTC video tracks.
use serde::{Deserialize, Serialize};

pub const SSRC_BASE: u32 = crate::voice::MediaKind::ScreenVideo.ssrc_base();
pub const AUDIO_SSRC_BASE: u32 = crate::voice::MediaKind::SystemAudio.ssrc_base();
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;
pub const H264_FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034";
pub const MAX_PACKETS_PER_SECOND: usize = 12_000;
pub const MAX_BYTES_PER_SECOND: usize = 12_000_000;
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SourceId {
    Monitor(u64),
    Window(u64),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub label: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub available: bool,
    pub sharing: bool,
    pub message: String,
    #[serde(default)]
    pub encoder: Option<String>,
    #[serde(default)]
    pub diagnostics: Diagnostics,
}

/// Aggregate counters only: never screen contents, addresses, SDP or credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Diagnostics {
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub events: Vec<(u64, String)>,
    pub elapsed_ms: u64,
    pub counters: std::collections::BTreeMap<String, u64>,
    pub timings: std::collections::BTreeMap<String, Timing>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    pub count: u64,
    pub total_us: u64,
    pub max_us: u64,
}

/// Requested capture targets; actual throughput depends on the source and host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quality {
    pub height: u32,
    pub fps: u32,
}
impl Default for Quality {
    fn default() -> Self {
        Self {
            height: 1080,
            fps: 30,
        }
    }
}
impl Quality {
    pub fn valid(self) -> bool {
        matches!(self.height, 720 | 1080 | 1440 | 2160) && matches!(self.fps, 15 | 30 | 60)
    }
    pub fn width(self) -> u32 {
        self.height * 16 / 9
    }
    /// Internal low-bandwidth modes are not selectable publishing presets.
    pub fn encoding_valid(self) -> bool {
        matches!(self.height, 360 | 720 | 1080 | 1440 | 2160)
            && matches!(self.fps, 5 | 15 | 30 | 60)
    }
    pub fn constrained(self, bitrate: u32) -> Self {
        let mut best = Self {
            height: 360,
            fps: 5,
        };
        for height in [360, 720, 1080, 1440, 2160] {
            for fps in [5, 15, 30, 60] {
                let candidate = Self { height, fps };
                // Prefer delivered pixels per second; avoid selecting 4K/5
                // when the same budget supports a more useful moving picture.
                let score = |q: Self| u64::from(q.height).pow(2) * u64::from(q.fps);
                if height <= self.height
                    && fps <= self.fps
                    && candidate.bitrate() <= bitrate
                    && score(candidate) >= score(best)
                {
                    best = candidate;
                }
            }
        }
        best
    }
    pub fn bitrate(self) -> u32 {
        let base = match self.height {
            360 => 1_000_000,
            720 => 2_500_000,
            1080 => 5_000_000,
            1440 => 9_000_000,
            _ => 16_000_000,
        };
        base * self.fps / 30
    }
}

pub const MIN_VIEW_BITRATE: u32 = 200_000;
pub const MAX_VIEW_BITRATE: u32 = 32_000_000;
pub const INITIAL_VIEW_BITRATE: u32 = 2_500_000;
pub const VIEW_LEASE_SECS: u64 = 3;

/// Full replacement of a receiver's active viewers, renewed over authenticated signaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub slot: usize,
    pub owner: crate::AccountId,
    pub epoch: u32,
    pub bitrate: u32,
}
impl Subscription {
    pub fn valid(self) -> bool {
        self.slot < crate::voice::ROOM_CAPACITY
            && (MIN_VIEW_BITRATE..=MAX_VIEW_BITRATE).contains(&self.bitrate)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_source_ids_preserve_native_handles_and_old_status_stays_readable() {
        let source = SourceId::Window(0x0000_0001_1234_5678);
        assert_eq!(
            serde_json::from_str::<SourceId>(&serde_json::to_string(&source).unwrap()).unwrap(),
            source
        );
        let status: Status =
            serde_json::from_str(r#"{"available":true,"sharing":false,"message":""}"#).unwrap();
        assert!(status.encoder.is_none());
    }
    #[test]
    fn quality_presets_are_bounded_and_untrusted_values_are_rejected() {
        for height in [720, 1080, 1440, 2160] {
            for fps in [15, 30, 60] {
                let q = Quality { height, fps };
                assert!(q.valid());
                assert!(q.width() <= MAX_WIDTH && q.height <= MAX_HEIGHT);
                assert!(q.bitrate() <= 32_000_000);
            }
        }
        for q in [
            Quality {
                height: 4320,
                fps: 60,
            },
            Quality {
                height: 1080,
                fps: 0,
            },
            Quality {
                height: 720,
                fps: 999,
            },
        ] {
            assert!(!q.valid());
        }
        assert!(Quality::default().valid());
    }
    #[test]
    fn adaptation_never_exceeds_receiver_budget_or_selected_quality() {
        for height in [720, 1080, 1440, 2160] {
            for fps in [15, 30, 60] {
                let maximum = Quality { height, fps };
                for budget in [
                    MIN_VIEW_BITRATE,
                    500_000,
                    1_250_000,
                    INITIAL_VIEW_BITRATE,
                    8_000_000,
                    MAX_VIEW_BITRATE,
                ] {
                    let actual = maximum.constrained(budget);
                    assert!(actual.encoding_valid());
                    assert!(actual.height <= height && actual.fps <= fps);
                    assert!(actual.bitrate() <= budget);
                }
                assert_eq!(maximum.constrained(MAX_VIEW_BITRATE), maximum);
            }
        }
        let reduced = Quality::default().constrained(MIN_VIEW_BITRATE);
        assert_eq!(
            reduced,
            Quality {
                height: 360,
                fps: 5
            }
        );
        assert!(
            !reduced.valid(),
            "internal adaptive modes are not user presets"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub slot: usize,
    pub owner: crate::AccountId,
    pub epoch: u32,
    pub viewer: u32,
}

/// Local native-to-WebView WebRTC negotiation. No account credentials or pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewOffer {
    pub watch: Watch,
    pub sdp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewAnswer {
    pub sdp: String,
}
