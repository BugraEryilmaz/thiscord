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
            Self::Good => (20, 0, 0, 1_000_000),
            Self::LowBandwidth => (120, 30, 0, 24_000),
            Self::HighLoss => (60, 20, 25, 1_000_000),
            Self::Unstable => (100, 100, 10, 48_000),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneLoop {
    Chimes,
    Pulse,
    Melody,
    Speech,
    #[default]
    EnglishClean,
    EnglishFan,
    EnglishKeyboard,
    EnglishCafe,
    EnglishStreet,
    EnglishRain,
    TurkishClean,
    TurkishFan,
    TurkishKeyboard,
    TurkishCafe,
    TurkishStreet,
    TurkishRain,
}
impl MicrophoneLoop {
    pub const ALL: [Self; 16] = [
        Self::EnglishClean,
        Self::TurkishClean,
        Self::EnglishFan,
        Self::TurkishFan,
        Self::EnglishKeyboard,
        Self::TurkishKeyboard,
        Self::EnglishCafe,
        Self::TurkishCafe,
        Self::EnglishStreet,
        Self::TurkishStreet,
        Self::EnglishRain,
        Self::TurkishRain,
        Self::Speech,
        Self::Chimes,
        Self::Pulse,
        Self::Melody,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Chimes => "Chimes",
            Self::Pulse => "Low pulse",
            Self::Melody => "Melody",
            Self::Speech => "Speech",
            Self::EnglishClean => "English · Quiet room",
            Self::EnglishFan => "English · Desk fan",
            Self::EnglishKeyboard => "English · Keyboard",
            Self::EnglishCafe => "English · Café conversation",
            Self::EnglishStreet => "English · Street traffic",
            Self::EnglishRain => "English · Rain",
            Self::TurkishClean => "Turkish · Quiet room",
            Self::TurkishFan => "Turkish · Desk fan",
            Self::TurkishKeyboard => "Turkish · Keyboard",
            Self::TurkishCafe => "Turkish · Café conversation",
            Self::TurkishStreet => "Turkish · Street traffic",
            Self::TurkishRain => "Turkish · Rain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub profile: ConnectionProfile,
    pub microphone_loop: MicrophoneLoop,
    #[serde(default)]
    pub audio: crate::audio::AudioSettings,
    #[serde(default)]
    pub scenario: Scenario,
    /// A held virtual PTT key. Released automatically when the helper exits.
    #[serde(default)]
    pub talking: bool,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    #[default]
    Clean,
    Quiet,
    Noise,
    Echo,
    DoubleTalk,
    Silence,
}
impl Scenario {
    pub const ALL: [Self; 6] = [
        Self::Clean,
        Self::Quiet,
        Self::Noise,
        Self::Echo,
        Self::DoubleTalk,
        Self::Silence,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "Clean microphone",
            Self::Quiet => "Quiet microphone",
            Self::Noise => "Background noise",
            Self::Echo => "Speaker echo only",
            Self::DoubleTalk => "Microphone and speaker echo",
            Self::Silence => "Silence",
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            profile: Default::default(),
            microphone_loop: MicrophoneLoop::EnglishClean,
            audio: Default::default(),
            scenario: Default::default(),
            talking: false,
        }
    }
}
impl std::ops::Deref for Settings {
    type Target = crate::audio::AudioSettings;
    fn deref(&self) -> &Self::Target {
        &self.audio
    }
}
impl std::ops::DerefMut for Settings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.audio
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.audio
            .validate()
            .map_err(|_| "Invalid bot voice settings")?;
        validate_audio(&self.audio)
    }
}
pub fn validate_audio(audio: &crate::audio::AudioSettings) -> Result<(), &'static str> {
    if audio
        .input
        .as_deref()
        .is_some_and(|s| s != "virtual-microphone")
        || audio
            .output
            .as_deref()
            .is_some_and(|s| s != "virtual-speakers")
        || audio.neural_echo_model.is_some()
    {
        return Err("Bots use virtual devices and bundled processing models");
    }
    Ok(())
}
/// Private, bounded parent/companion protocol. Never exposed as a public socket.
#[derive(Serialize, Deserialize)]
pub enum HostMessage {
    Start {
        token: String,
        guild: GuildId,
        channel: ChannelId,
        bot: Bot,
    },
    Signal(String),
    Settings(Settings),
}
#[derive(Serialize, Deserialize)]
pub enum PeerMessage {
    Signal(String),
    Ready(Result<(), StartupFailure>),
    Snapshot(Box<Bot>),
}
#[derive(Serialize, Deserialize)]
pub enum StartupFailure {
    Unavailable,
    Forbidden,
    Limited,
}
pub const IPC_LIMIT: usize = 256 * 1024;

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
    #[serde(default)]
    pub connection_id: Option<crate::RequestId>,
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
    fn speech_catalog_has_stable_unique_wire_names_and_keeps_legacy_loops() {
        let mut names = std::collections::HashSet::new();
        for kind in MicrophoneLoop::ALL {
            let name = serde_json::to_value(kind).unwrap();
            assert!(names.insert(name.as_str().unwrap().to_owned()));
            assert_eq!(
                serde_json::from_value::<MicrophoneLoop>(name).unwrap(),
                kind
            );
        }
        for legacy in ["speech", "chimes", "pulse", "melody"] {
            assert!(names.contains(legacy));
        }
        assert_eq!(
            names.iter().filter(|s| s.starts_with("english_")).count(),
            6
        );
        assert_eq!(
            names.iter().filter(|s| s.starts_with("turkish_")).count(),
            6
        );
        assert_eq!(
            Settings::default().microphone_loop,
            MicrophoneLoop::EnglishClean
        );
    }
    #[test]
    fn normal_voice_settings_round_trip_and_bot_devices_are_bounded() {
        let mut settings = Settings::default();
        settings.audio.noise_suppression = true;
        settings.audio.noise_suppression_model =
            crate::audio::NoiseSuppressionModel::DeepFilterNet3;
        settings.audio.automatic_gain = true;
        settings.audio.echo_cancellation = true;
        settings.audio.neural_echo = true;
        settings.audio.packet_redundancy = true;
        settings.audio.deep_redundancy = true;
        settings.audio.adaptive_fec = true;
        settings.audio.mode = crate::audio::TransmitMode::PushToTalk;
        settings.audio.input = Some("virtual-microphone".into());
        settings.audio.output = Some("virtual-speakers".into());
        settings.talking = true;
        settings.scenario = Scenario::DoubleTalk;
        settings.validate().unwrap();
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
        assert_eq!(settings, restored);
        settings.audio.neural_echo_model = Some("/server/private/model".into());
        assert!(settings.validate().is_err());
        settings.audio.neural_echo_model = None;
        settings.audio.input = Some("physical-device".into());
        assert!(settings.validate().is_err());
    }
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
                audio: crate::audio::AudioSettings {
                    input_gain: f32::NAN,
                    ..Default::default()
                },
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                audio: crate::audio::AudioSettings {
                    output_volume: 2.1,
                    ..Default::default()
                },
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
