//! Native audio control contracts; PCM never crosses the WebView bridge.
use serde::{Deserialize, Serialize};

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
    pub id: String,
    pub label: String,
    pub volume: f32,
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
    pub dropped_samples: u64,
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
