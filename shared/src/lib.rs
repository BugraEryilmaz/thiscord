//! Transport contracts shared by the desktop UI and backend.
//! Keep this crate independent of Leptos, Tauri, Axum, and Diesel.

use serde::{Deserialize, Serialize};

pub const HEALTH_PATH: &str = "/api/v1/health";

/// Process liveness only; this does not report database readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
}
