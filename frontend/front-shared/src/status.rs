use serde::{Deserialize, Serialize};

use crate::FromEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Status {
    OnCall(String),
    Online,
    Connecting,
    Offline,
}
impl FromEvent for Status {}