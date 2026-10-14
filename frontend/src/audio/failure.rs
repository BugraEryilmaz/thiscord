//! Processing errors retain a user-facing message separately from log-safe fields.
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Failure {
    pub stage: &'static str,
    pub code: &'static str,
    pub message: String,
    opus_code: Option<i32>,
    opus_function: Option<&'static str>,
}
impl Failure {
    pub fn new(stage: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            code,
            message: message.into(),
            opus_code: None,
            opus_function: None,
        }
    }
    pub fn sonora(stage: &'static str, error: sonora::Error) -> Self {
        let code = match error {
            sonora::Error::InvalidSampleRate { .. } => "invalid_sample_rate",
            sonora::Error::InvalidChannelCount { .. } => "invalid_channel_count",
            sonora::Error::ChannelMismatch { .. } => "channel_mismatch",
            sonora::Error::StreamParameterClamped => "stream_parameter_clamped",
        };
        Self::new(stage, code, error.to_string())
    }
    pub fn opus(stage: &'static str, error: opus::Error) -> Self {
        Self {
            opus_code: Some(error.code() as i32),
            opus_function: Some(error.function()),
            ..Self::new(stage, "opus", error.to_string())
        }
    }
    pub fn details(&self) -> Value {
        json!({"stage":self.stage,"code":self.code,"opus_code":self.opus_code,"opus_function":self.opus_function})
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Failure {}
impl From<Failure> for String {
    fn from(error: Failure) -> Self {
        error.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_omit_dynamic_messages_and_keep_actual_stage() {
        let error = Failure::new(
            "deep_filter",
            "inference_failed",
            "private model path or payload",
        );
        let details = error.details();
        assert_eq!(details["stage"], "deep_filter");
        assert_eq!(details["code"], "inference_failed");
        assert!(!details.to_string().contains("private"));
        assert_eq!(
            Failure::sonora("echo_delay", sonora::Error::StreamParameterClamped).details()["code"],
            "stream_parameter_clamped"
        );
    }
}
