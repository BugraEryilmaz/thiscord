use serde::{Deserialize, Serialize};

use crate::{RequestId, validation::FieldError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    ValidationFailed,
    NotFound,
    MethodNotAllowed,
    ServiceUnavailable,
    InternalError,
}

/// Safe public error payload. Internal causes belong in correlated server logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldError>,
}
