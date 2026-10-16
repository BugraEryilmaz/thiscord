//! Control-only camera feedback; video never crosses command IPC.
use serde::{Deserialize, Serialize};
pub const MAX_BITRATE: u32 = 1_500_000;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub alive: bool,
    /// Zero pauses network encoding; local preview may continue.
    pub bitrate: u32,
}
