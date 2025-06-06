use crate::audio::AudioCommand;

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
}
