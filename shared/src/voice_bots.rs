//! Ephemeral instance-owner voice test participants, never login accounts.
use crate::{AccountId, ChannelId, GuildId};
use serde::{Deserialize, Serialize};

pub const PATH: &str = "/api/v1/voice/bots";
pub const MAX_BOTS: usize = 16;
pub const LIFETIME_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionProfile {
    #[default]
    Good,
    LowBandwidth,
    HighLoss,
    Unstable,
}
impl ConnectionProfile {
    pub const ALL: [Self; 4] = [
        Self::Good,
        Self::LowBandwidth,
        Self::HighLoss,
        Self::Unstable,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Good => "Good connection",
            Self::LowBandwidth => "Low bandwidth",
            Self::HighLoss => "High packet loss",
            Self::Unstable => "Unstable connection",
        }
    }
    /// Aggregate video downlink budget, separate from audio to preserve voice traffic.
    pub fn video_bits(self) -> u32 {
        match self {
            Self::Good | Self::HighLoss => 32_000_000,
            Self::LowBandwidth => 500_000,
            Self::Unstable => 2_000_000,
        }
    }
    /// Simulated one-way media delay, jitter, loss percent and wire budget.
    pub fn parameters(self) -> (u64, u64, u32, u32) {
        match self {
            Self::Good => (20, 0, 0, 96_000),
            Self::LowBandwidth => (120, 30, 0, 24_000),
            Self::HighLoss => (60, 20, 25, 96_000),
            Self::Unstable => (100, 100, 10, 48_000),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneLoop {
    #[default]
    Chimes,
    Pulse,
    Melody,
}
impl MicrophoneLoop {
    pub const ALL: [Self; 3] = [Self::Chimes, Self::Pulse, Self::Melody];
    pub fn label(self) -> &'static str {
        match self {
            Self::Chimes => "Chimes",
            Self::Pulse => "Low pulse",
            Self::Melody => "Melody",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub profile: ConnectionProfile,
    pub microphone_loop: MicrophoneLoop,
    pub input_gain: f32,
    pub output_volume: f32,
    pub muted: bool,
    pub deafened: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            profile: Default::default(),
            microphone_loop: Default::default(),
            input_gain: 1.0,
            output_volume: 1.0,
            muted: false,
            deafened: false,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.input_gain.is_finite()
            || !(1.0..=10.0).contains(&self.input_gain)
            || !self.output_volume.is_finite()
            || !(0.0..=2.0).contains(&self.output_volume)
        {
            return Err("Invalid bot audio levels");
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub command: Command,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    List {},
    Add {
        settings: Settings,
    },
    Update {
        bot_id: AccountId,
        revision: u64,
        settings: Settings,
    },
    Remove {
        bot_id: AccountId,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    pub id: AccountId,
    pub name: String,
    pub revision: u64,
    pub settings: Settings,
    pub sent_packets: u64,
    pub dropped_packets: u64,
    pub received_packets: u64,
    pub output_peak: f32,
    #[serde(default)]
    pub video: Vec<VideoReceiver>,
    #[serde(default)]
    pub received_system_audio_packets: u64,
}
/// Current camera/screen reception, scoped to an owner and stream incarnation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoReceiver {
    pub kind: crate::voice::MediaKind,
    pub slot: usize,
    pub owner: AccountId,
    pub epoch: u32,
    pub bitrate: u32,
    pub received_packets: u64,
    pub dropped_packets: u64,
    pub missing_packets: u64,
    pub decoded_frames: u64,
    pub decode_errors: u64,
    pub keyframe_requests: u64,
    pub width: usize,
    pub height: usize,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Response {
    pub bots: Vec<Bot>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_names_and_bounds() {
        let value = serde_json::to_value(Command::Add {
            settings: Settings::default(),
        })
        .unwrap();
        assert_eq!(value["action"], "add");
        assert_eq!(value["settings"]["profile"], "good");
        assert!(
            serde_json::from_value::<Command>(
                serde_json::json!({"action":"add","settings":{},"token":"x"})
            )
            .is_err()
        );
        assert!(
            Settings {
                input_gain: f32::NAN,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                output_volume: 2.1,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
