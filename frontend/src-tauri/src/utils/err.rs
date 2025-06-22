use my_web_rtc::{WebRTCError, WebSocketMessage};

use crate::{audio::AudioCommand, commands::logout};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Not Implemented")]
    NotImplemented,
    #[error("CPAL getting default stream config error: {0}")]
    CpaldefaultStreamConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("No input device found")]
    NoInputDevice,
    #[error("No output device found")]
    NoOutputDevice,
    #[error("CPAL Pause stream error: {0}")]
    MuteMicrophone(#[from] cpal::PauseStreamError),
    #[error("CPAL build stream error: {0}")]
    CpalBuildStream(#[from] cpal::BuildStreamError),
    #[error("CPAL play stream error: {0}")]
    CpalPlayStream(#[from] cpal::PlayStreamError),
    #[error("Send error: {0}")]
    SendError(#[from] std::sync::mpsc::SendError<AudioCommand>),
    #[error("Update error: {0}")]
    UpdateError(#[from] tauri_plugin_updater::Error),
    #[error("WebSocket error: {0}")]
    WebSocketError(#[from] my_web_rtc::WebSocketError),
    #[error("Tokio tungstenite error: {0}")]
    TokioTungsteniteError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Native TLS error: {0}")]
    NativeTlsError(#[from] native_tls::Error),
    #[error("WebRTC error: {0}")]
    WebRTCError(#[from] my_web_rtc::Error),
    #[error("WebRTC error: {0}")]
    WebRTCInternalError(#[from] WebRTCError),
    #[error("Tokio Channel error: {0}")]
    TokioChannelError(#[from] tokio::sync::mpsc::error::SendError<WebSocketMessage>),
}

pub async fn handle_auth_error(
    response: Result<reqwest::Response, reqwest::Error>,
    app: tauri::AppHandle,
) -> Result<reqwest::Response, reqwest::Error> {
    let response = if let Ok(resp) = response {
        resp.error_for_status()
    } else {
        response
    };
    if let Err(e) = &response {
        if let Some(status) = e.status() {
            if status.as_str() == "401" {
                tracing::warn!("Unauthorized access detected, attempting to log out.");
                let _ = logout(app.clone()).await;
            }
        }
    }
    response
}
