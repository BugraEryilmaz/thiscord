//! Small native Rust bot SDK. See `bots/README.md` and the runnable examples.
//! No desktop runtime, microphone, audio device or DSP is required.

mod client;
mod commands;
mod subscription;
pub mod voice;

pub use client::{Client, Manager};
pub use commands::{Bot, Context, parse_command};
pub use subscription::Subscription;
pub use thiscord_shared::{AccountId, ChannelId, ClientMessageId, GuildId, MessageId};
pub use thiscord_shared::{bots::BotInfo, chat::ChatMessage};
pub use voice::Voice;

/// Optional native credential storage. Services can inject a token through their
/// secret manager instead. Neither path writes a plaintext credential file.
pub struct Credentials;
impl Credentials {
    fn entry(server: &str, name: &str) -> Result<keyring::Entry, Error> {
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::message("Invalid bot credential name"));
        }
        let key = format!(
            "{}|{name}",
            endpoint(server)?.origin().ascii_serialization()
        );
        keyring::Entry::new("thiscord-scripting-bots", &key)
            .map_err(|_| Error::message("OS credential store unavailable"))
    }
    pub fn store(server: &str, name: &str, token: &str) -> Result<(), Error> {
        Self::entry(server, name)?.set_password(token).map_err(|_| {
            Error::message(
                "Cannot save bot token in OS credential store; rotate to issue a replacement",
            )
        })
    }
    pub fn load(server: &str, name: &str) -> Result<String, Error> {
        Self::entry(server, name)?
            .get_password()
            .map_err(|_| Error::message("Cannot load bot token from OS credential store"))
    }
    pub fn delete(server: &str, name: &str) -> Result<(), Error> {
        Self::entry(server, name)?
            .delete_credential()
            .map_err(|_| Error::message("Cannot delete saved bot token"))
    }
    pub fn from_env() -> Result<String, Error> {
        match std::env::var("THISCORD_BOT_TOKEN") {
            Ok(token) => Ok(token),
            Err(_) => Self::load(
                &std::env::var("THISCORD_BOT_SERVER")
                    .map_err(|_| Error::message("Set THISCORD_BOT_SERVER"))?,
                &std::env::var("THISCORD_BOT_NAME").map_err(|_| {
                    Error::message("Set THISCORD_BOT_NAME or inject THISCORD_BOT_TOKEN")
                })?,
            ),
        }
    }
}

use futures_util::SinkExt;
use std::{fmt, time::Duration};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite};

/// Safe to log: excludes message bodies, credentials, SDP and remote error text.
#[derive(Debug)]
pub struct Error {
    message: &'static str,
    pub code: Option<thiscord_shared::ErrorCode>,
    pub request_id: Option<thiscord_shared::RequestId>,
}
impl Error {
    pub fn message(message: &'static str) -> Self {
        Self {
            message,
            code: None,
            request_id: None,
        }
    }
    pub(crate) fn api(error: thiscord_shared::ApiError) -> Self {
        Self {
            message: "The server rejected the bot request",
            code: Some(error.code),
            request_id: Some(error.request_id),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)?;
        if let Some(code) = &self.code {
            write!(f, " ({code:?})")?;
        }
        if let Some(id) = &self.request_id {
            write!(f, " [request {id}]")?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn endpoint(value: &str) -> Result<url::Url, Error> {
    let url = url::Url::parse(value).map_err(|_| Error::message("Invalid server URL"))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::message(
            "Use an HTTPS server origin, or HTTP on loopback",
        ));
    }
    Ok(url)
}

pub(crate) async fn connect_socket(base: &str, origin: &str, path: &str) -> Result<Socket, Error> {
    use tungstenite::client::IntoClientRequest;
    let mut url = endpoint(base)?
        .join(path)
        .map_err(|_| Error::message("Invalid socket path"))?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| Error::message("Invalid socket URL"))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| Error::message("Invalid socket request"))?;
    request.headers_mut().insert(
        "origin",
        origin
            .parse()
            .map_err(|_| Error::message("Invalid Origin header"))?,
    );
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(128 * 1024))
        .max_frame_size(Some(128 * 1024));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Error::message("Socket connection timed out"))?
    .map_err(|_| Error::message("Socket connection failed; check server URL and allowed Origin"))?;
    Ok(socket)
}

pub(crate) async fn send_frame(
    socket: &mut Socket,
    frame: &impl serde::Serialize,
) -> Result<(), Error> {
    let text =
        serde_json::to_string(frame).map_err(|_| Error::message("Cannot encode socket frame"))?;
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(tungstenite::Message::Text(text.into())),
    )
    .await
    .map_err(|_| Error::message("Socket send timed out"))?
    .map_err(|_| Error::message("Socket disconnected"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_require_secure_origin_and_no_embedded_secrets() {
        for bad in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com?token=secret",
            "file:///tmp/x",
        ] {
            assert!(endpoint(bad).is_err());
        }
        for good in [
            "https://example.com",
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            assert!(endpoint(good).is_ok(), "{good}");
        }
    }
}
