mod my_web_rtc;

use futures_util::{stream::{SplitSink, SplitStream}, SinkExt, StreamExt as _};
pub use my_web_rtc::WebRTCConnection;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;

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
    #[error("Axum Websocket error: {0}")]
    WebSocket(#[from] axum::Error),
    #[error("Tungstenite error: {0}")]
    Tungstenite(#[from] tungstenite::Error),
    #[error("WebSocket not connected")]
    WebSocketNotConnected,
    #[error("Native TLS error: {0}")]
    NativeTls(#[from] native_tls::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalingMessage {
    Offer(webrtc::peer_connection::sdp::session_description::RTCSessionDescription),
    Answer(webrtc::peer_connection::sdp::session_description::RTCSessionDescription),
    IceCandidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit),
}

#[derive(Debug)]
pub enum Writer {
    Server(SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>),
    Client(
        SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
    ),
}

impl Writer {
    pub async fn send(&mut self, message: SignalingMessage) -> Result<(), Error> {
        let serialized = serde_json::to_string(&message)?;
        println!("Sending message: {}", serialized);
        match self {
            Writer::Server(sender) => {
                let serialized = axum::extract::ws::Utf8Bytes::from(serialized);
                sender
                    .send(axum::extract::ws::Message::Text(serialized))
                    .await
                    .map_err(Error::WebSocket)
            }
            Writer::Client(sender) => {
                let serialized = tokio_tungstenite::tungstenite::Utf8Bytes::from(serialized);
                sender
                    .send(tokio_tungstenite::tungstenite::Message::Text(serialized))
                    .await
                    .map_err(Error::Tungstenite)
            }
        }
    }
}

pub enum Reader {
    Server(SplitStream<axum::extract::ws::WebSocket>),
    Client(
        SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
}

impl Reader {
    pub async fn next(&mut self) -> Result<Option<SignalingMessage>, Error> {
        match self {
            Reader::Server(receiver) => {
                let msg = receiver.next().await;
                println!("Received message: {:?}", msg);
                if let Some(message) = msg {
                    match message? {
                        axum::extract::ws::Message::Text(text) => {
                            let msg: SignalingMessage = serde_json::from_str(&text)?;
                            Ok(Some(msg))
                        }
                        _ => Ok(None),
                    }
                } else {
                    Ok(None)
                }
            }
            Reader::Client(receiver) => {
                if let Some(message) = receiver.next().await {
                    match message? {
                        tokio_tungstenite::tungstenite::Message::Text(text) => {
                            let msg: SignalingMessage = serde_json::from_str(&text)?;
                            Ok(Some(msg))
                        }
                        _ => Ok(None),
                    }
                } else {
                    Ok(None)
                }
            }
        }
    }
}