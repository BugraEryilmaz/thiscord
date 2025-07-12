use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioDevices {
    pub mics: Vec<String>,
    pub mic: Option<String>,
    pub speakers: Vec<String>,
    pub speaker: Option<String>,
}