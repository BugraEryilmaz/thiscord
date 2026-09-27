use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCode {
    Required,
    TooLong,
    InvalidFormat,
    OutOfRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub field: String,
    pub code: ValidationCode,
}

/// Validates required text by Unicode scalar count, without silently trimming or
/// rewriting input. Domain-specific syntax rules belong alongside each DTO.
pub fn validate_text(field: &str, value: &str, max_chars: usize) -> Result<(), FieldError> {
    let code = if value.trim().is_empty() {
        Some(ValidationCode::Required)
    } else if value.chars().count() > max_chars {
        Some(ValidationCode::TooLong)
    } else {
        None
    };
    match code {
        Some(code) => Err(FieldError {
            field: field.into(),
            code,
        }),
        None => Ok(()),
    }
}
