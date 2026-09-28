//! Desktop update status crosses Tauri IPC; signing keys and download URLs do not.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading,
    Installing,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub available_version: Option<String>,
    pub phase: UpdatePhase,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub message: String,
}
