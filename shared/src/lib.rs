//! Transport contracts shared by the desktop UI and backend.
//! Keep this crate independent of Leptos, Tauri, Axum, and Diesel.

use serde::{Deserialize, Serialize};

pub mod account;
pub mod error;
pub mod ids;
pub mod pagination;
pub mod validation;

pub use error::{ApiError, ErrorCode};
pub use ids::{AccountId, InstanceId, RequestId, SessionId};

/// RFC 3339 on the wire, normalized to UTC. No OS clock is needed in shared code.
pub type Timestamp = chrono::DateTime<chrono::Utc>;

pub const HEALTH_PATH: &str = "/api/v1/health";
pub const READY_PATH: &str = "/api/v1/ready";
pub const REQUEST_ID_HEADER: &str = "x-request-id";

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

/// Returned only after the database and foundational schema have been queried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessResponse {
    pub status: ReadinessStatus,
    pub instance_id: InstanceId,
    pub checked_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ready,
}
