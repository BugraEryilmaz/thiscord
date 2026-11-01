//! Guild soundboard contracts. The backend decodes and normalizes uploads; clients
//! only receive bounded mono 48 kHz signed 16-bit little-endian PCM for playback.
use crate::{AccountId, ChannelId, GuildId, SoundId, Timestamp};
use serde::{Deserialize, Serialize};

pub const SOUNDBOARD_PATH: &str = "/api/v1/soundboard";
/// Fixed per-guild allowance for uploaded sounds, independent of instance roles.
pub const MAX_SOUNDS: usize = 24;
pub const MAX_UPLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_UPLOAD_BASE64: usize = MAX_UPLOAD_BYTES.div_ceil(3) * 4;
pub const SAMPLE_RATE: u32 = 48_000;
pub const MAX_DURATION_MS: u32 = 5_000;
pub const MAX_SAMPLES: usize = (SAMPLE_RATE as usize) * (MAX_DURATION_MS as usize) / 1000;
pub const MAX_PCM_BYTES: usize = MAX_SAMPLES * 2;
pub const MAX_NAME_CHARS: usize = 32;
pub const MAX_EMOJI_CHARS: usize = 8;

/// Authenticated GET returning `application/octet-stream` PCM for a voice channel.
pub fn audio_path(guild: GuildId, channel: ChannelId, sound: SoundId) -> String {
    format!("{SOUNDBOARD_PATH}/{guild}/{channel}/{sound}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sound {
    pub id: SoundId,
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
    pub duration_ms: u32,
    #[serde(default)]
    pub created_by: Option<AccountId>,
    pub created_at: Timestamp,
    /// Available in every guild; not counted toward `MAX_SOUNDS` and not deletable.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

// Uploads carry user media; deliberately no Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SoundboardRequest {
    /// With a voice channel, requires UseSoundboard there; without, ManageGuild.
    List {
        guild_id: GuildId,
        #[serde(default)]
        channel_id: Option<ChannelId>,
    },
    /// WAV, MP3, Ogg Vorbis or FLAC up to `MAX_UPLOAD_BYTES`. Longer audio is
    /// trimmed to `MAX_DURATION_MS` after leading silence.
    Upload {
        guild_id: GuildId,
        revision: i64,
        name: String,
        #[serde(default)]
        emoji: Option<String>,
        audio_base64: String,
    },
    Delete {
        guild_id: GuildId,
        revision: i64,
        sound_id: SoundId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundboardState {
    pub guild_id: GuildId,
    /// Guild revision checked by management writes.
    pub revision: i64,
    /// Built-in sounds first, then uploads in creation order.
    pub sounds: Vec<Sound>,
    pub can_manage: bool,
    /// Maximum uploaded sounds; built-in sounds are extra.
    pub limit: u32,
}

pub fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    name.trim() == name
        && (1..=MAX_NAME_CHARS).contains(&count)
        && !name.chars().any(char::is_control)
}

pub fn valid_emoji(emoji: &str) -> bool {
    let count = emoji.chars().count();
    (1..=MAX_EMOJI_CHARS).contains(&count)
        && !emoji
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c.is_ascii_alphanumeric())
}

/// Validates a playback download and returns its samples.
pub fn decode_pcm(bytes: &[u8]) -> Option<Vec<i16>> {
    if bytes.is_empty() || bytes.len() > MAX_PCM_BYTES || !bytes.len().is_multiple_of(2) {
        return None;
    }
    Some(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect(),
    )
}

pub fn encode_pcm(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_are_tagged_and_reject_unknown_fields() {
        let id = "00000000-0000-0000-0000-000000000001";
        let wire = json!({"action":"list","guild_id":id,"channel_id":id});
        let request: SoundboardRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), wire);
        let legacy: SoundboardRequest =
            serde_json::from_value(json!({"action":"list","guild_id":id})).unwrap();
        assert!(matches!(
            legacy,
            SoundboardRequest::List {
                channel_id: None,
                ..
            }
        ));
        assert!(
            serde_json::from_value::<SoundboardRequest>(
                json!({"action":"delete","guild_id":id,"revision":1,"sound_id":id,"force":true})
            )
            .is_err()
        );
    }

    #[test]
    fn names_emoji_and_pcm_are_bounded() {
        assert!(valid_name("airhorn"));
        assert!(!valid_name(" airhorn"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(MAX_NAME_CHARS + 1)));
        assert!(!valid_name("a\nb"));
        assert!(valid_emoji("🦆"));
        assert!(valid_emoji("👍🏽"));
        assert!(!valid_emoji("a"));
        assert!(!valid_emoji("🦆 🦆"));
        assert!(!valid_emoji(&"🦆".repeat(MAX_EMOJI_CHARS + 1)));
        let samples = [0, 1, -1, i16::MAX, i16::MIN];
        assert_eq!(decode_pcm(&encode_pcm(&samples)).unwrap(), samples);
        assert!(decode_pcm(&[]).is_none());
        assert!(decode_pcm(&[0]).is_none());
        assert!(decode_pcm(&vec![0; MAX_PCM_BYTES + 2]).is_none());
        assert_eq!(MAX_SAMPLES, 240_000);
        const { assert!(MAX_UPLOAD_BASE64 >= MAX_UPLOAD_BYTES * 4 / 3) };
    }
}
