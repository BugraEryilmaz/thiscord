//! Native audio control contracts; PCM never crosses the WebView bridge.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransmitMode {
    #[default]
    VoiceActivity,
    PushToTalk,
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
    pub automatic_gain: bool,
    pub echo_cancellation: bool,
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
            automatic_gain: false,
            echo_cancellation: false,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStatus {
    pub running: bool,
    pub input_level: f32,
    /// Microphone peak before echo/noise processing. No PCM crosses IPC.
    #[serde(default)]
    pub raw_input_level: f32,
    /// Adaptation restarts caused by lost capture or playback-reference blocks.
    #[serde(default)]
    pub processing_resets: u64,
    pub transmitting: bool,
    pub message: String,
    pub streams: Vec<StreamLevel>,
    pub dropped_samples: u64,
    pub underrun_samples: u64,
}
