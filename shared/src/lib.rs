mod my_web_rtc;
pub mod models;
pub mod schema;

pub use my_web_rtc::WebRTCConnection;
pub use ringbuf::HeapCons;
pub use ringbuf::HeapRb;
pub use ringbuf::traits::Consumer;
pub use ringbuf::traits::Observer;
pub use ringbuf::traits::Producer;
pub use ringbuf::traits::Split;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
pub use webrtc::Error as WebRTCError;
pub use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
pub use webrtc::rtp::packet::Packet;
pub use webrtc::stats;
pub use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
pub use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::{
    ice_transport::ice_candidate::RTCIceCandidateInit,
    peer_connection::sdp::session_description::RTCSessionDescription,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Not Implemented")]
    NotImplemented,
    #[error("WebRTC error: {0}")]
    WebRTC(#[from] webrtc::error::Error),
    #[error("Opus error: {0}")]
    Opus(#[from] opus::Error),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("ICE candidate error: {0}")]
    IceCandidate(#[from] webrtc::ice::Error),
    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("WebSocket not connected")]
    WebSocketNotConnected,
    #[error("Native TLS error: {0}")]
    NativeTls(#[from] native_tls::Error),
    #[error("Mutex error")]
    Mutex,
}

impl<T> From<std::sync::PoisonError<std::sync::MutexGuard<'_, T>>> for Error {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<'_, T>>) -> Self {
        Error::Mutex
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSocketMessage {
    JoinAudioChannel { server_id: Uuid, channel_id: Uuid },
    WebRTCOffer(RTCSessionDescription),
    WebRTCAnswer(RTCSessionDescription),
    IceCandidate(RTCIceCandidateInit),
    DisconnectFromAudioChannel,
    Disconnect,
    Error { err: WebSocketError },
}

#[derive(Debug, Clone, Serialize, Deserialize, Error)]
pub enum WebSocketError {
    #[error("Not Authorized")]
    NotAuthorized,
    #[error("Not Found")]
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalingMessage {
    Offer(webrtc::peer_connection::sdp::session_description::RTCSessionDescription),
    Answer(webrtc::peer_connection::sdp::session_description::RTCSessionDescription),
    IceCandidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit),
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IsClosed {
    Closed,
    NotClosed,
}
