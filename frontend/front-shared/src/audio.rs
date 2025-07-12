use serde::{Deserialize, Serialize};

pub mod last_used_devices;
use last_used_devices::LastUsedAudioDevicesWString;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioDevices {
    pub mics: Vec<String>,
    pub speakers: Vec<String>,
    pub last_used_devices: Option<LastUsedAudioDevicesWString>,
}