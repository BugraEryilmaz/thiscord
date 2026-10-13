//! Native audio control contracts; PCM never crosses the WebView bridge.
use serde::{Deserialize, Serialize};

/// Local self controls; deafen suppresses transmission without changing manual mute.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioToggle {
    Mute,
    Deafen,
}
impl AudioToggle {
    pub fn apply(self, settings: &mut AudioSettings) {
        match self {
            Self::Mute => settings.muted = !settings.muted,
            Self::Deafen => settings.deafened = !settings.deafened,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioShortcuts {
    /// Empty strings disable individual shortcuts. Native registration validates keys.
    pub mute: String,
    pub deafen: String,
}
impl Default for AudioShortcuts {
    fn default() -> Self {
        Self {
            mute: "Control+Shift+M".into(),
            deafen: "Control+Shift+D".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AudioShortcutStatus {
    pub shortcuts: AudioShortcuts,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransmitMode {
    #[default]
    VoiceActivity,
    PushToTalk,
}

/// The enable switch is separate so disabling processing preserves the selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseSuppressionModel {
    #[default]
    Sonora,
    DeepFilterNet3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    pub input: Option<String>,
    pub output: Option<String>,
    pub output_volume: f32,
    pub activation_threshold: f32,
    pub mode: TransmitMode,
    pub global_push_to_talk: bool,
    pub noise_suppression: bool,
    pub noise_suppression_model: NoiseSuppressionModel,
    pub automatic_gain: bool,
    pub echo_cancellation: bool,
    /// Experimental residual estimator, independent of the noise suppressor.
    pub neural_echo: bool,
    /// Optional local override; None uses the bundled model. Never sent to the backend.
    pub neural_echo_model: Option<String>,
    pub muted: bool,
    pub deafened: bool,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            output_volume: 1.0,
            activation_threshold: 0.015,
            mode: TransmitMode::VoiceActivity,
            global_push_to_talk: false,
            noise_suppression: false,
            noise_suppression_model: NoiseSuppressionModel::Sonora,
            automatic_gain: false,
            echo_cancellation: false,
            neural_echo: false,
            neural_echo_model: None,
            muted: false,
            deafened: false,
        }
    }
}
impl AudioSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.output_volume.is_finite()
            || !(0.0..=2.0).contains(&self.output_volume)
            || !self.activation_threshold.is_finite()
            || !(0.001..=0.5).contains(&self.activation_threshold)
            || self
                .input
                .iter()
                .chain(self.output.iter())
                .any(|s| s.len() > 2048)
            || self
                .neural_echo_model
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 4096 || s.contains('\0'))
        {
            return Err("Invalid audio settings");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub input: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamLevel {
    /// Recent decoded voice activity before local playback gain.
    #[serde(default)]
    pub speaking: bool,
    pub id: String,
    #[serde(default)]
    pub target: Option<SpeakerVolumeTarget>,
    pub label: String,
    pub volume: f32,
}
/// Stable local preference identity; slots and display names can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpeakerVolumeTarget {
    pub guild_id: crate::GuildId,
    pub account_id: crate::AccountId,
    /// Local playback preference, independent of the account's microphone.
    #[serde(default)]
    pub shared_audio: bool,
}
/// Local, numeric diagnostics only; never includes PCM or recorded speech.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoDiagnostics {
    pub reference_level: f32,
    /// AEC's linear-filter estimate, not total audible echo attenuation.
    pub filter_reduction_db: Option<f32>,
    pub estimated_delay_ms: Option<i32>,
    /// Percentage of mono input samples at full scale in the last completed second.
    pub clipped_input_percent: f32,
    pub automatic_gain: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStatus {
    #[serde(default)]
    pub quality: Option<AudioQuality>,
    /// Local diagnostic recording state only; audio bytes never cross IPC.
    #[serde(default)]
    pub recording: Option<AudioRecordingStatus>,
    pub running: bool,
    pub input_level: f32,
    /// Microphone peak before echo/noise processing. No PCM crosses IPC.
    #[serde(default)]
    pub raw_input_level: f32,
    /// Adaptation restarts caused by lost capture or playback-reference blocks.
    #[serde(default)]
    pub processing_resets: u64,
    #[serde(default)]
    pub echo: Option<EchoDiagnostics>,
    pub transmitting: bool,
    pub message: String,
    pub streams: Vec<StreamLevel>,
    /// Legacy sum of capture/reference queue sample drops. Prefer quality metrics.
    pub dropped_samples: u64,
    /// Legacy name: per-stream playback starvation samples, never device xruns.
    pub underrun_samples: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRecordingStatus {
    pub active: bool,
    pub saving: bool,
    pub elapsed_ms: u64,
    pub directory: String,
    pub dropped_blocks: u64,
    pub error: Option<String>,
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn quality_is_backward_compatible_optional_and_contains_only_numeric_metadata() {
        let legacy = serde_json::json!({"running":false,"input_level":0.0,"transmitting":false,"message":"","streams":[],"dropped_samples":0,"underrun_samples":0});
        let mut status: AudioStatus = serde_json::from_value(legacy).unwrap();
        assert!(status.quality.is_none());
        status.quality = Some(AudioQuality::default());
        let value = serde_json::to_value(&status).unwrap();
        assert!(value["quality"]["end_to_end_loss_percent"].is_null());
        let again: AudioStatus = serde_json::from_value(value).unwrap();
        assert!(again.quality.unwrap().sfu_hop_rtt_ms.is_none());
        let partial: AudioQuality = serde_json::from_str(
            r#"{"metrics":{"recent":{"received_packets":3}},"future_field":42}"#,
        )
        .unwrap();
        assert_eq!(partial.metrics.recent.received_packets, 3);
        let text = format!("{:?}", AudioQuality::default());
        for forbidden in [
            "account_id",
            "payload",
            "credential",
            "token",
            "sdp",
            "model_path",
        ] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn deafen_round_trip_preserves_manual_mute() {
        let mut settings = AudioSettings::default();
        AudioToggle::Mute.apply(&mut settings);
        AudioToggle::Deafen.apply(&mut settings);
        assert!(settings.muted && settings.deafened);
        AudioToggle::Deafen.apply(&mut settings);
        assert!(settings.muted && !settings.deafened);
        AudioToggle::Mute.apply(&mut settings);
        AudioToggle::Deafen.apply(&mut settings);
        assert!(!settings.muted && settings.deafened);
    }

    #[test]
    fn shortcut_defaults_and_toggle_wire_values_are_stable() {
        let old: AudioSettings = serde_json::from_str("{}").unwrap();
        assert!(!old.muted && !old.deafened);
        let defaults: AudioShortcuts = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults, AudioShortcuts::default());
        assert_eq!(
            serde_json::to_string(&AudioToggle::Mute).unwrap(),
            "\"mute\""
        );
        assert_eq!(
            serde_json::to_string(&AudioToggle::Deafen).unwrap(),
            "\"deafen\""
        );
    }
}

/// Numeric local status only. See docs/audio-quality.md for units and scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityCounts {
    pub capture_overflow_samples: u64,
    pub reference_overflow_samples: u64,
    pub input_samples: u64,
    pub input_clipped_samples: u64,
    pub output_samples: u64,
    pub output_clipped_samples: u64,
    pub outgoing_offered_packets: u64,
    pub outgoing_full_packets: u64,
    pub outgoing_expired_packets: u64,
    pub outgoing_invalidated_packets: u64,
    pub outgoing_sent_packets: u64,
    pub outgoing_failed_packets: u64,
    pub incoming_queue_dropped_packets: u64,
    pub received_packets: u64,
    pub duplicate_packets: u64,
    pub reordered_packets: u64,
    pub late_packets: u64,
    pub jitter_overflow_packets: u64,
    pub resync_discarded_packets: u64,
    pub invalid_packets: u64,
    pub sequence_gap_packets: u64,
    pub fec_attempts: u64,
    pub plc_samples: u64,
    pub decoded_samples: u64,
    pub decode_errors: u64,
    pub pcm_overflow_samples: u64,
    pub starvation_samples: u64,
    pub empty_unknown_samples: u64,
    pub intentional_silence_samples: u64,
    pub pcm_discarded_samples: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityTiming {
    pub calls: u64,
    pub deadline_misses: u64,
    pub p50_us: Option<u64>,
    pub p95_us: Option<u64>,
    pub p99_us: Option<u64>,
    pub max_us: Option<u64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityMetrics {
    pub total: QualityCounts,
    pub recent: QualityCounts,
    pub window_ms: u64,
    pub capture: QualityTiming,
    pub render: QualityTiming,
    pub encode: QualityTiming,
    pub decode: QualityTiming,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamQuality {
    /// Local reusable slot, not an account or transport identifier.
    pub slot: usize,
    pub metrics: QualityMetrics,
    pub jitter_packets: usize,
    pub pcm_samples: usize,
    pub buffered_playout_ms: f64,
    pub verified_fec_packets: Option<u64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioQuality {
    pub metrics: QualityMetrics,
    pub streams: Vec<StreamQuality>,
    pub capture_blocks: usize,
    pub reference_blocks: usize,
    pub sfu_hop_rtt_ms: Option<f64>,
    pub sfu_hop_received_packets: Option<u64>,
    pub sfu_hop_lost_packets: Option<i64>,
    pub end_to_end_loss_percent: Option<f64>,
    pub end_to_end_latency_ms: Option<f64>,
    pub limiter_reduction_db: Option<f32>,
}
