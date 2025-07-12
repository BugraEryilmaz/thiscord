use cpal::{
    traits::{DeviceTrait, HostTrait},
    Device,
};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct LastUsedAudioDevices {
    pub mic: Option<Device>,
    pub speaker: Option<Device>,
}

impl LastUsedAudioDevices {
    pub fn new(mic: Option<Device>, speaker: Option<Device>) -> Self {
        LastUsedAudioDevices { mic, speaker }
    }

    pub fn get_from_db_or_default(conn: &mut SqliteConnection) -> Result<Self, diesel::result::Error> {
        let mut ret = LastUsedAudioDevicesWString::get_from_db(conn).map(LastUsedAudioDevices::from)?;
        if ret.mic.is_none() {
            ret.mic = cpal::default_host().default_input_device();
        }
        if ret.speaker.is_none() {
            ret.speaker = cpal::default_host().default_output_device();
        }
        Ok(ret)
    }

    pub fn save_to_db(&mut self, conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
        let devices: LastUsedAudioDevicesWString = self.clone().into();
        devices.save_to_db(conn)
    }
}

impl Default for LastUsedAudioDevices {
    fn default() -> Self {
        let host = cpal::default_host();
        LastUsedAudioDevices {
            mic: host.default_input_device(),
            speaker: host.default_output_device(),
        }
    }
}

impl Serialize for LastUsedAudioDevices {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let devices: LastUsedAudioDevicesWString = self.clone().into();
        devices.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LastUsedAudioDevices {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let devices: LastUsedAudioDevicesWString =
            LastUsedAudioDevicesWString::deserialize(deserializer)?;
        Ok(devices.into())
    }
}

impl std::fmt::Debug for LastUsedAudioDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let audio_devices: LastUsedAudioDevicesWString = self.clone().into();
        audio_devices.fmt(f)
    }
}

impl From<LastUsedAudioDevices> for LastUsedAudioDevicesWString {
    fn from(devices: LastUsedAudioDevices) -> Self {
        let mic = devices
            .mic
            .as_ref()
            .and_then(|d| d.name().ok());
        let speaker = devices
            .speaker
            .as_ref()
            .and_then(|d| d.name().ok());
        LastUsedAudioDevicesWString {
            id: Some(1),
            mic,
            speaker,
        }
    }
}

impl From<LastUsedAudioDevicesWString> for LastUsedAudioDevices {
    fn from(devices: LastUsedAudioDevicesWString) -> Self {
        let host = cpal::default_host();
        let mics = host.input_devices();
        let mic = match devices.mic {
            Some(mic_name) => match mics {
                Ok(mut devices) => devices
                    .find(|d| d.name().unwrap_or_default() == mic_name)
                    .map_or(host.default_input_device(), |d| Some(d)),
                Err(_) => host.default_input_device(),
            },
            None => host.default_input_device(),
        };
        let speakers = host.output_devices();
        let speaker = match devices.speaker {
            Some(speaker_name) => match speakers {
                Ok(mut devices) => devices
                    .find(|d| d.name().unwrap_or_default() == speaker_name)
                    .map_or(host.default_output_device(), |d| Some(d)),
                Err(_) => host.default_output_device(),
            },
            None => host.default_output_device(),
        };
        LastUsedAudioDevices { mic, speaker }
    }
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, Queryable, Insertable, AsChangeset)]
#[diesel(table_name = crate::schema::last_used_audio_devices)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct LastUsedAudioDevicesWString {
    pub id: Option<i32>,
    pub mic: Option<String>,
    pub speaker: Option<String>,
}

impl LastUsedAudioDevicesWString {
    pub fn get_from_db(conn: &mut SqliteConnection) -> Result<Self, diesel::result::Error> {
        use crate::schema::last_used_audio_devices::dsl::*;
        let result = last_used_audio_devices
            .filter(id.eq(1))
            .first::<LastUsedAudioDevicesWString>(conn)
            .optional()?;
        Ok(result.unwrap_or_else(|| LastUsedAudioDevicesWString::default()))
    }
    pub fn save_to_db(&self, conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
        use crate::schema::last_used_audio_devices::dsl::*;
        diesel::insert_into(last_used_audio_devices)
            .values(self)
            .on_conflict(id)
            .do_update()
            .set(self)
            .execute(conn)?;
        Ok(())
    }
}
