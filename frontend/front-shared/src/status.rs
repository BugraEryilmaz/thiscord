use serde::{Deserialize, Serialize};

use crate::FromEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Status {
    OnCall(String, CallStatus),
    Online,
    Connecting,
    Offline,
}
impl FromEvent for Status {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallStatus {
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}
