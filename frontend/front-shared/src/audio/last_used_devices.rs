use serde::{Deserialize, Serialize};


#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct LastUsedAudioDevicesWString {
    pub id: Option<i32>,
    pub mic: Option<String>,
    pub speaker: Option<String>,
    pub mic_boost: Option<i32>,
    pub speaker_boost: Option<i32>,
}

