//! Instance-admin control of a specific live desktop voice connection.
use crate::{RequestId, audio::*};
use serde::{Deserialize, Serialize};

pub const PATH: &str = "/api/v1/admin/audio";
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 32 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub connection_id: RequestId,
    pub command: Command,
}

/// Deliberate allowlist, never arbitrary IPC, filesystem access or recording.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    AudioCurrent {},
    AudioDevices {},
    AudioStatus {},
    AudioSave {
        settings: AudioSettings,
        expected: AudioSettings,
    },
    AudioToggle {
        action: AudioToggle,
    },
    AudioVolume {
        stream: usize,
        gain: f32,
        target: Option<SpeakerVolumeTarget>,
    },
    AudioShortcuts {},
    AudioShortcutsSave {
        settings: AudioShortcuts,
    },
    OverlayEnabled {},
    OverlayEnable {
        enabled: bool,
    },
    AudioDiagnosticsEnabled {},
    AudioDiagnosticsEnable {
        enabled: bool,
    },
    AudioDiagnosticsDownload {},
}
impl Command {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::AudioSave { settings, expected } => {
                settings.validate()?;
                expected.validate()
            }
            Self::AudioVolume {
                stream,
                gain,
                target,
            } if *stream >= 24
                || !gain.is_finite()
                || !(0.0..=2.0).contains(gain)
                || target.is_none() =>
            {
                Err("Invalid speaker volume")
            }
            Self::AudioShortcutsSave { settings }
                if settings.mute.len() > 128 || settings.deafen.len() > 128 =>
            {
                Err("Invalid shortcut")
            }
            _ => Ok(()),
        }
    }
}

pub type Reply = Result<serde_json::Value, String>;

#[derive(Clone, Serialize, Deserialize)]
pub struct DiagnosticFile {
    pub name: String,
    pub contents: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_arbitrary_commands_and_invalid_settings() {
        for command in [
            "audio_debug_start",
            "audio_pressed",
            "audio_diagnostics_folder",
            "voice_join",
        ] {
            assert!(
                serde_json::from_value::<Command>(serde_json::json!({"command":command})).is_err()
            );
        }
        let settings = AudioSettings {
            output_volume: 3.0,
            ..Default::default()
        };
        assert!(
            Command::AudioSave {
                settings,
                expected: AudioSettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_str::<Command>(r#"{"command":"audio_current","path":"secret"}"#)
                .is_err()
        );
        let command: Command =
            serde_json::from_str(r#"{"command":"audio_diagnostics_download"}"#).unwrap();
        assert!(command.validate().is_ok());
    }
}
