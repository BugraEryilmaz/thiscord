#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Not Implemented")]
    NotImplemented,
    #[error("CPAL getting default stream config error: {0}")]
    CpaldefaultStreamConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("No input device found")]
    NoInputDevice,
    #[error("CPAL build stream error: {0}")]
    CpalBuildStream(#[from] cpal::BuildStreamError),
    #[error("CPAL play stream error: {0}")]
    CpalPlayStream(#[from] cpal::PlayStreamError),
}