//! Allowlisted error metadata only: library Display/Debug can contain frames,
//! URLs, HTTP bodies, headers and credentials.
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};

pub fn io(error: &std::io::Error) -> Value {
    json!({"kind":"io", "io_kind":format!("{:?}", error.kind()), "os_code":error.raw_os_error()})
}

pub fn socket(error: &Error) -> Value {
    match error {
        Error::Io(error) => io(error),
        Error::Protocol(error) => json!({"kind":"protocol", "code":protocol(error)}),
        Error::Capacity(error) => {
            use tokio_tungstenite::tungstenite::error::CapacityError;
            match error {
                CapacityError::TooManyHeaders => {
                    json!({"kind":"capacity", "code":"too_many_headers"})
                }
                CapacityError::MessageTooLong { size, max_size } => {
                    json!({"kind":"capacity", "code":"message_too_long", "size":size, "limit":max_size})
                }
            }
        }
        Error::Http(response) => json!({"kind":"http", "status":response.status().as_u16()}),
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

fn protocol(error: &ProtocolError) -> &'static str {
    match error {
        ProtocolError::WrongHttpMethod => "wrong_http_method",
        ProtocolError::WrongHttpVersion => "wrong_http_version",
        ProtocolError::MissingConnectionUpgradeHeader => "missing_connection_upgrade",
        ProtocolError::MissingUpgradeWebSocketHeader => "missing_websocket_upgrade",
        ProtocolError::MissingSecWebSocketVersionHeader => "missing_websocket_version",
        ProtocolError::MissingSecWebSocketKey => "missing_websocket_key",
        ProtocolError::SecWebSocketAcceptKeyMismatch => "accept_key_mismatch",
        ProtocolError::SecWebSocketSubProtocolError(_) => "subprotocol",
        ProtocolError::JunkAfterRequest => "junk_after_request",
        ProtocolError::CustomResponseSuccessful => "custom_response_successful",
        ProtocolError::InvalidHeader(_) => "invalid_header",
        ProtocolError::HandshakeIncomplete => "handshake_incomplete",
        ProtocolError::HttparseError(_) => "http_parse",
        ProtocolError::SendAfterClosing => "send_after_closing",
        ProtocolError::ReceivedAfterClosing => "received_after_closing",
        ProtocolError::NonZeroReservedBits => "reserved_bits",
        ProtocolError::UnmaskedFrameFromClient => "unmasked_client_frame",
        ProtocolError::MaskedFrameFromServer => "masked_server_frame",
        ProtocolError::FragmentedControlFrame => "fragmented_control_frame",
        ProtocolError::ControlFrameTooBig => "control_frame_too_big",
        ProtocolError::UnknownControlFrameType(_) => "unknown_control_frame_type",
        ProtocolError::UnknownDataFrameType(_) => "unknown_data_frame_type",
        ProtocolError::UnexpectedContinueFrame => "unexpected_continue_frame",
        ProtocolError::ExpectedFragment(_) => "expected_fragment",
        ProtocolError::ResetWithoutClosingHandshake => "reset_without_closing_handshake",
        ProtocolError::InvalidOpcode(_) => "invalid_opcode",
        ProtocolError::InvalidCloseSequence => "invalid_close_sequence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_failure_kind_without_private_payloads() {
        let secret = "secret-token-and-private-message";
        let errors = [
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                secret,
            )),
            Error::WriteBufferFull(Box::new(tokio_tungstenite::tungstenite::Message::Text(
                secret.into(),
            ))),
            Error::Utf8(secret.into()),
            Error::Http(Box::new(
                tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(502)
                    .header("set-cookie", secret)
                    .body(Some(secret.as_bytes().to_vec()))
                    .unwrap(),
            )),
        ];
        for error in &errors {
            assert!(!socket(error).to_string().contains(secret));
        }
        assert_eq!(socket(&errors[0])["io_kind"], "ConnectionReset");
        assert_eq!(socket(&errors[3])["status"], 502);
        assert_eq!(
            socket(&Error::Protocol(
                ProtocolError::ResetWithoutClosingHandshake
            ))["code"],
            "reset_without_closing_handshake"
        );
    }
}
