//! Per-upgrade lifecycle records. Never format arbitrary transport errors or frames.
use axum::extract::ws::{Message, WebSocket};
use serde_json::{Value, json};
use std::{error::Error as _, time::Instant};
use thiscord_shared::RequestId;

pub(super) struct Socket {
    inner: WebSocket,
    pub id: RequestId,
    pub stage: &'static str,
    started: Instant,
    last_received: Option<Instant>,
    received: u64,
    sent: u64,
    reason: Option<&'static str>,
}
impl Socket {
    pub fn new(inner: WebSocket, id: RequestId) -> Self {
        tracing::info!(request_id = %id, "voice connection started");
        Self {
            inner,
            id,
            stage: "join",
            started: Instant::now(),
            last_received: None,
            received: 0,
            sent: 0,
            reason: None,
        }
    }
    /// Retain the first failure; cleanup must not mask it with a send error.
    pub fn end(&mut self, reason: &'static str) {
        self.reason.get_or_insert(reason);
    }
    pub async fn recv(&mut self) -> Option<Result<Message, axum::Error>> {
        let result = self.inner.recv().await;
        match &result {
            Some(Ok(message)) => {
                self.received += 1;
                self.last_received = Some(Instant::now());
                if let Message::Close(frame) = message {
                    self.end("client_close");
                    tracing::info!(request_id = %self.id, close_code = ?frame.as_ref().map(|f| f.code), "voice signaling close received");
                }
            }
            Some(Err(error)) => {
                self.end("signaling_receive_failed");
                tracing::warn!(request_id = %self.id, stage = self.stage, error = %error_details(error), "voice signaling receive failed");
            }
            None => self.end("signaling_eof"),
        }
        result
    }
    pub async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        let result = self.inner.send(message).await;
        match &result {
            Ok(()) => self.sent += 1,
            Err(error) => {
                self.end("signaling_send_failed");
                tracing::warn!(request_id = %self.id, stage = self.stage, error = %error_details(error), "voice signaling send failed");
            }
        }
        result
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        tracing::info!(request_id = %self.id, reason = self.reason.unwrap_or("cancelled"), stage = self.stage,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            signaling_age_ms = ?self.last_received.map(|t| t.elapsed().as_millis() as u64),
            received_messages = self.received, sent_messages = self.sent,
            "voice connection ended");
    }
}

fn error_details(error: &axum::Error) -> Value {
    let mut source = error.source();
    // Bound traversal even if a third-party error has a broken/cyclic source chain.
    for _ in 0..8 {
        let Some(error) = source else { break };
        if let Some(error) = error.downcast_ref::<tungstenite::Error>() {
            return websocket_error(error);
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            return io_error(error);
        }
        source = error.source();
    }
    json!({"kind":"unknown"})
}
fn io_error(error: &std::io::Error) -> Value {
    json!({"kind":"io","io_kind":format!("{:?}",error.kind()),"os_code":error.raw_os_error()})
}
fn websocket_error(error: &tungstenite::Error) -> Value {
    use tungstenite::{
        Error,
        error::{CapacityError, ProtocolError},
    };
    match error {
        Error::Io(error) => io_error(error),
        Error::Protocol(error) => json!({"kind":"protocol", "code":match error {
            ProtocolError::ResetWithoutClosingHandshake => "reset_without_closing_handshake",
            ProtocolError::SendAfterClosing => "send_after_closing",
            ProtocolError::ReceivedAfterClosing => "received_after_closing",
            ProtocolError::UnmaskedFrameFromClient => "unmasked_client_frame",
            ProtocolError::MaskedFrameFromServer => "masked_server_frame",
            ProtocolError::NonZeroReservedBits => "reserved_bits",
            ProtocolError::FragmentedControlFrame => "fragmented_control_frame",
            ProtocolError::ControlFrameTooBig => "control_frame_too_big",
            ProtocolError::InvalidCloseSequence => "invalid_close_sequence",
            ProtocolError::UnexpectedContinueFrame => "unexpected_continue_frame",
            ProtocolError::ExpectedFragment(_) => "expected_fragment",
            ProtocolError::UnknownControlFrameType(_) => "unknown_control_frame_type",
            ProtocolError::UnknownDataFrameType(_) => "unknown_data_frame_type",
            ProtocolError::InvalidOpcode(_) => "invalid_opcode",
            _ => "handshake",
        }}),
        Error::Capacity(CapacityError::MessageTooLong { size, max_size }) => {
            json!({"kind":"capacity","code":"message_too_long","size":size,"limit":max_size})
        }
        Error::Capacity(_) => json!({"kind":"capacity","code":"too_many_headers"}),
        Error::Http(response) => json!({"kind":"http","status":response.status().as_u16()}),
        other => json!({"kind":match other {
            Error::ConnectionClosed => "connection_closed",
            Error::AlreadyClosed => "already_closed",
            Error::Tls(_) => "tls",
            Error::WriteBufferFull(_) => "write_buffer_full",
            Error::Utf8(_) => "utf8",
            Error::AttackAttempt => "attack_attempt",
            Error::Url(_) => "url",
            Error::HttpFormat(_) => "http_format",
            _ => "unknown",
        }}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn actual_socket_closes_log_reason_and_id_without_close_body() {
        use axum::{Router, extract::WebSocketUpgrade, routing::get};
        use futures_util::SinkExt;
        use tracing::instrument::WithSubscriber;
        for graceful in [false, true] {
            let capture = Capture::default();
            let writer = capture.clone();
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            let dispatch = tracing::Dispatch::new(subscriber);
            let id: RequestId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
            let (done, finished) = tokio::sync::oneshot::channel();
            let done = Arc::new(Mutex::new(Some(done)));
            let app = Router::new().route(
                "/",
                get(move |ws: WebSocketUpgrade| {
                    let done = done.clone();
                    let dispatch = dispatch.clone();
                    async move {
                        ws.on_upgrade(move |socket| {
                            async move {
                                let mut socket = Socket::new(socket, id);
                                let _ = socket.recv().await;
                                // A later generic cleanup reason cannot replace the original.
                                socket.end("cleanup");
                                drop(socket);
                                done.lock().unwrap().take().unwrap().send(()).unwrap();
                            }
                            .with_subscriber(dispatch)
                        })
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let (mut client, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
                .await
                .unwrap();
            if graceful {
                client
                    .send(tungstenite::Message::Close(Some(
                        tungstenite::protocol::CloseFrame {
                            code: tungstenite::protocol::frame::coding::CloseCode::Away,
                            reason: "private-close-body".into(),
                        },
                    )))
                    .await
                    .unwrap();
            }
            drop(client);
            tokio::time::timeout(std::time::Duration::from_secs(2), finished)
                .await
                .unwrap()
                .unwrap();
            server.abort();
            let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
            assert!(output.contains(&id.to_string()), "{output}");
            assert!(output.contains("voice connection ended"), "{output}");
            assert!(
                output.contains(if graceful {
                    "client_close"
                } else {
                    "signaling_receive_failed"
                }),
                "{output}"
            );
            assert!(!output.contains("private-close-body"));
            if graceful {
                assert!(output.contains("1001"), "{output}");
            } else {
                assert!(
                    output.contains("reset_without_closing_handshake"),
                    "{output}"
                );
            }
        }
    }
    #[test]
    fn wrapped_errors_preserve_codes_without_logging_secrets() {
        let private = "token, SDP, HTTP body or private message";
        for error in [
            tungstenite::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                private,
            )),
            tungstenite::Error::WriteBufferFull(Box::new(tungstenite::Message::Text(
                private.into(),
            ))),
            tungstenite::Error::Utf8(private.into()),
        ] {
            let details = error_details(&axum::Error::new(error));
            assert!(!details.to_string().contains(private));
            assert_ne!(details["kind"], "unknown");
        }
        let error = axum::Error::new(tungstenite::Error::Protocol(
            tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ));
        assert_eq!(
            error_details(&error)["code"],
            "reset_without_closing_handshake"
        );
    }
}
