//! Local-only speaker levels. Call disk methods on a serialized blocking task,
//! never the media worker or device callbacks.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
use thiscord_shared::{AccountId, GuildId, audio::SpeakerVolumeTarget};

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    guilds: BTreeMap<GuildId, BTreeMap<AccountId, f32>>,
    #[serde(default)]
    shared_audio: BTreeMap<GuildId, BTreeMap<AccountId, f32>>,
}

pub struct Profile {
    pub guild_id: GuildId,
    gains: BTreeMap<AccountId, f32>,
    shared_audio: BTreeMap<AccountId, f32>,
}
impl Profile {
    pub fn gain(&self, account: AccountId, shared_audio: bool) -> f32 {
        let gains = if shared_audio {
            &self.shared_audio
        } else {
            &self.gains
        };
        gains.get(&account).copied().unwrap_or(1.0)
    }
    pub fn set(&mut self, target: SpeakerVolumeTarget, gain: f32) -> Result<(), String> {
        validate(gain)?;
        if self.guild_id != target.guild_id {
            return Err("Voice channel changed; adjust the current speaker instead".into());
        }
        let gains = if target.shared_audio {
            &mut self.shared_audio
        } else {
            &mut self.gains
        };
        gains.insert(target.account_id, gain);
        Ok(())
    }
}
fn validate(gain: f32) -> Result<(), String> {
    if !gain.is_finite() || !(0.0..=2.0).contains(&gain) {
        return Err("Volume must be between 0 and 200%".into());
    }
    Ok(())
}
impl Store {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("Cannot read speaker volume settings".into()),
        };
        use std::io::Read;
        let mut data = Vec::new();
        file.take(524_289)
            .read_to_end(&mut data)
            .map_err(|_| "Cannot read speaker volume settings")?;
        if data.len() > 524_288 {
            return Err("Speaker volume settings file is too large".into());
        }
        let store: Self =
            serde_json::from_slice(&data).map_err(|_| "Invalid speaker volume settings file")?;
        store.validate()?;
        Ok(store)
    }
    fn validate(&self) -> Result<(), String> {
        if self.guilds.len() + self.shared_audio.len() > 4096
            || self
                .guilds
                .values()
                .chain(self.shared_audio.values())
                .map(BTreeMap::len)
                .sum::<usize>()
                > 4096
        {
            return Err("Too many saved speaker volumes".into());
        }
        for gain in self
            .guilds
            .values()
            .chain(self.shared_audio.values())
            .flat_map(BTreeMap::values)
        {
            validate(*gain)?;
        }
        Ok(())
    }
    pub fn profile(&self, guild_id: GuildId) -> Profile {
        Profile {
            guild_id,
            gains: self.guilds.get(&guild_id).cloned().unwrap_or_default(),
            shared_audio: self
                .shared_audio
                .get(&guild_id)
                .cloned()
                .unwrap_or_default(),
        }
    }
    pub fn save(
        &mut self,
        path: &Path,
        target: SpeakerVolumeTarget,
        gain: f32,
    ) -> Result<(), String> {
        validate(gain)?;
        let guilds = if target.shared_audio {
            &mut self.shared_audio
        } else {
            &mut self.guilds
        };
        guilds
            .entry(target.guild_id)
            .or_default()
            .insert(target.account_id, gain);
        self.validate()?;
        let data = serde_json::to_vec(self).map_err(|_| "Invalid speaker volume settings")?;
        std::fs::create_dir_all(path.parent().ok_or("Invalid settings directory")?)
            .map_err(|_| "Cannot create settings directory")?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, data).map_err(|_| "Cannot save speaker volume settings")?;
        std::fs::rename(temporary, path).map_err(|_| "Cannot save speaker volume settings")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(guild: u8, account: u8) -> SpeakerVolumeTarget {
        SpeakerVolumeTarget {
            shared_audio: false,
            guild_id: format!("00000000-0000-0000-0000-{guild:012}")
                .parse()
                .unwrap(),
            account_id: format!("00000000-0000-0000-0000-{account:012}")
                .parse()
                .unwrap(),
        }
    }
    #[test]
    fn legacy_voice_preferences_and_shared_audio_are_independent() {
        let voice = target(1, 1);
        let screen = SpeakerVolumeTarget {
            shared_audio: true,
            ..voice
        };
        let legacy = serde_json::json!({"guilds": {voice.guild_id.to_string(): {voice.account_id.to_string(): 0.4}}});
        let mut store: Store = serde_json::from_value(legacy).unwrap();
        let mut profile = store.profile(voice.guild_id);
        assert_eq!(profile.gain(voice.account_id, false), 0.4);
        assert_eq!(profile.gain(voice.account_id, true), 1.0);
        profile.set(screen, 0.0).unwrap();
        assert_eq!(profile.gain(voice.account_id, false), 0.4);
        assert_eq!(profile.gain(voice.account_id, true), 0.0);
        let path = std::env::temp_dir().join(format!(
            "thiscord-screen-volumes-{}.json",
            std::process::id()
        ));
        store.save(&path, screen, 0.0).unwrap();
        let loaded = Store::load(&path).unwrap().profile(voice.guild_id);
        assert_eq!(loaded.gain(voice.account_id, false), 0.4);
        assert_eq!(loaded.gain(voice.account_id, true), 0.0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn disk_roundtrip_isolates_guilds_and_accounts_and_replaces_existing_file() {
        let path =
            std::env::temp_dir().join(format!("thiscord-volumes-{}.json", std::process::id()));
        let a = target(1, 1);
        let b = target(2, 1);
        let mut store = Store::default();
        store.save(&path, a, 0.0).unwrap();
        store.save(&path, b, 1.8).unwrap();
        let loaded = Store::load(&path).unwrap();
        assert_eq!(loaded.profile(a.guild_id).gain(a.account_id, false), 0.0);
        assert_eq!(loaded.profile(b.guild_id).gain(b.account_id, false), 1.8);
        assert_eq!(
            loaded
                .profile(a.guild_id)
                .gain(target(1, 2).account_id, false),
            1.0
        );
        let mut profile = loaded.profile(a.guild_id);
        assert!(profile.set(b, 0.5).is_err());
        assert!(store.save(&path, a, f32::NAN).is_err());
        assert!(store.save(&path, a, 2.1).is_err());
        assert_eq!(
            Store::load(&path)
                .unwrap()
                .profile(a.guild_id)
                .gain(a.account_id, false),
            0.0
        );
        std::fs::write(&path, b"broken").unwrap();
        assert!(Store::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            Store::load(&path)
                .unwrap()
                .profile(a.guild_id)
                .gain(a.account_id, false),
            1.0
        );
    }
}
