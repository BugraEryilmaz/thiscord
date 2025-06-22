use my_web_rtc::WebSocketMessage;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(#[from] diesel::result::Error),
    #[error("DBPool error: {0}")]
    R2D2(#[from] r2d2::Error),
    #[error("Username or password is incorrect")]
    InvalidCredentials,
    #[error("Room is full")]
    RoomFull,
    #[error("Password hash error: {0}")]
    PasswordHash(String),
    #[error("File error: {0}")]
    File(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Email error: {0}")]
    Email(#[from] lettre::transport::smtp::Error),
    #[error("Invalid Activation Code")]
    InvalidActivationCode,
    #[error("WebRTC error: {0}")]
    WebRTC(#[from] my_web_rtc::Error),
    #[error("UserNotFound in the room")]
    UserNotFoundInRoom,
    #[error("Permission denied")]
    PermissionDenied,
    #[error("Not implemented")]
    NotImplemented,
    #[error("Invalid file name: {0}")]
    InvalidFileName(String),
    #[error("ConnectionStringGenerationFailed")]
    ConnectionStringGenerationFailed,
    #[error("WebRTC error: {0}")]
    WebRTCinternal(#[from] my_web_rtc::WebRTCError),
    #[error("WebRTC connection is not initialized")]
    WebRTCConnectionNotInitialized,
    #[error("Axum Websocket error: {0}")]
    WebSocket(#[from] axum::Error),
    #[error("Tokio mpsc error: {0}")]
    TokioMpsc(#[from] tokio::sync::mpsc::error::SendError<WebSocketMessage>),
    #[error("WebSocket error")]
    WebSocketError(#[from] my_web_rtc::WebSocketError),
}

impl From<argon2::password_hash::Error> for Error {
    fn from(err: argon2::password_hash::Error) -> Self {
        Error::PasswordHash(err.to_string())
    }
}
