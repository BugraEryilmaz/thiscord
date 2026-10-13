//! Native voice signaling has a separate network path from the WebView.
//! Diagnose each stage without logging account tokens, HTTP bodies or SDP.
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{io, net::SocketAddr, time::Duration};
use thiscord_frontend::audio::connection::Connection;
use thiscord_frontend::audio::reconnect::Failure;
pub(crate) mod diagnostics;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Error, client::IntoClientRequest, protocol::WebSocketConfig},
};

const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type Sender = futures_util::stream::SplitSink<Socket, tokio_tungstenite::tungstenite::Message>;

fn endpoint(base: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid voice server URL")?;
    if !url.username().is_empty() || url.password().is_some() || url.host().is_none() {
        return Err("Invalid voice server URL".into());
    }
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return Err("Invalid voice server URL".into()),
    };
    url.set_scheme(scheme)
        .map_err(|_| "Invalid voice server URL")?;
    url.set_path(thiscord_shared::voice::VOICE_PATH);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn network_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        " Check Thiscord in System Settings > Privacy & Security > Local Network, then reopen the app."
    } else {
        " Check the server address, VPN and firewall."
    }
}

// Interleave families, preserving preference, so an unreachable first address
// cannot consume the entire deadline before another family gets a chance.
fn candidates(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut unique = Vec::new();
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    let prefer_v6 = unique.first().is_some_and(SocketAddr::is_ipv6);
    let (preferred, alternate): (Vec<_>, Vec<_>) = unique
        .into_iter()
        .partition(|address| address.is_ipv6() == prefer_v6);
    let mut result = Vec::new();
    for index in 0..preferred.len().max(alternate.len()) {
        result.extend(preferred.get(index).copied());
        result.extend(alternate.get(index).copied());
    }
    result.truncate(16);
    result
}

async fn dial(addresses: Vec<SocketAddr>, stagger: Duration) -> io::Result<TcpStream> {
    let mut attempts = FuturesUnordered::new();
    for (index, address) in candidates(addresses).into_iter().enumerate() {
        attempts.push(async move {
            if index > 0 {
                tokio::time::sleep(stagger * index as u32).await;
            }
            TcpStream::connect(address).await
        });
    }
    let mut last_error = io::Error::new(io::ErrorKind::AddrNotAvailable, "No server addresses");
    while let Some(result) = attempts.next().await {
        match result {
            Ok(socket) => return Ok(socket), // Dropping other futures cancels their attempts.
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

fn handshake_error(error: Error, endpoint: &url::Url) -> Failure {
    match error {
        Error::Http(response) => {
            let status = response.status().as_u16();
            let message = format!("Voice WebSocket rejected by {endpoint} (HTTP {status}).");
            if status == 429 || status >= 500 { Failure::temporary(message) } else { message.into() }
        }
        Error::Tls(_) => format!("Voice TLS verification or negotiation failed for {endpoint}. Check the Mac/PC clock, server certificate and any network TLS inspection.").into(),
        Error::Io(error) => Failure::temporary(format!("Voice TLS/WebSocket connection to {endpoint} failed ({:?}).{}", error.kind(), network_hint())),
        _ => format!("Invalid voice WebSocket handshake from {endpoint}.").into(),
    }
}

fn record_request_id(
    connection: &Connection,
    headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
) {
    // Accept only the server-generated UUID, including for a rejected upgrade.
    if let Some(id) = headers
        .get(thiscord_shared::REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
    {
        connection.set_request_id(id);
    }
}

pub async fn connect(base: &str) -> Result<Socket, Failure> {
    connect_for(base, &Connection::default()).await
}

pub async fn connect_for(base: &str, connection: &Connection) -> Result<Socket, Failure> {
    connect_with_deadlines(
        base,
        DNS_TIMEOUT,
        TCP_TIMEOUT,
        HANDSHAKE_TIMEOUT,
        connection,
    )
    .await
}

async fn connect_with_deadlines(
    base: &str,
    dns_timeout: Duration,
    tcp_timeout: Duration,
    handshake_timeout: Duration,
    connection: &Connection,
) -> Result<Socket, Failure> {
    let started = std::time::Instant::now();
    let endpoint = endpoint(base)?;
    let port = endpoint
        .port_or_known_default()
        .ok_or("Invalid voice port")?;
    let addresses = match endpoint.host().ok_or("Invalid voice host")? {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Domain(host) => timeout(dns_timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| {
                connection.event(
                    "voice_connect_failed",
                    serde_json::json!({"stage":"dns", "error":{"kind":"timeout"}}),
                );
                Failure::temporary(format!(
                    "Voice DNS lookup timed out for {host}. Check DNS and VPN settings."
                ))
            })?
            .map_err(|error| {
                connection.event(
                    "voice_connect_failed",
                    serde_json::json!({"stage":"dns", "error":diagnostics::io(&error)}),
                );
                Failure::temporary(format!(
                    "Cannot resolve voice server {host}. Check DNS and VPN settings."
                ))
            })?
            .collect(),
    };
    let stream = timeout(tcp_timeout, dial(addresses, Duration::from_millis(250)))
        .await
        .map_err(|_| {
            connection.event(
                "voice_connect_failed",
                serde_json::json!({"stage":"tcp", "error":{"kind":"timeout"}}),
            );
            Failure::temporary(format!(
                "Voice TCP connection to {endpoint} timed out.{}",
                network_hint()
            ))
        })?
        .map_err(|error| {
            connection.event(
                "voice_connect_failed",
                serde_json::json!({"stage":"tcp", "error":diagnostics::io(&error)}),
            );
            Failure::temporary(format!(
                "Voice TCP connection to {endpoint} failed ({:?}).{}",
                error.kind(),
                network_hint()
            ))
        })?;
    let _ = stream.set_nodelay(true);
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(|_| "Invalid voice WebSocket URL")?;
    request
        .headers_mut()
        .insert("origin", "http://tauri.localhost".parse().unwrap());
    // Keep the hostname in the request for SNI/certificate validation even when
    // the TCP connection was made to a resolved address. Never bypass TLS checks.
    let (socket, response) = timeout(
        handshake_timeout,
        tokio_tungstenite::client_async_tls_with_config(
            request,
            stream,
            Some(WebSocketConfig::default().max_message_size(Some(128 * 1024)).max_frame_size(Some(128 * 1024))),
            None,
        ),
    )
    .await
    .map_err(|_| {
        connection.event("voice_connect_failed", serde_json::json!({"stage":"handshake", "error":{"kind":"timeout"}}));
        Failure::temporary(format!("Voice TLS/WebSocket handshake with {endpoint} timed out. Check VPN, proxy and firewall settings."))
    })?
    .map_err(|error| {
        if let Error::Http(response) = &error {
            record_request_id(connection, response.headers());
        }
        connection.event("voice_connect_failed", serde_json::json!({"stage":"handshake", "error":diagnostics::socket(&error)}));
        handshake_error(error, &endpoint)
    })?;
    record_request_id(connection, response.headers());
    connection.event(
        "voice_signaling_connected",
        serde_json::json!({"elapsed_ms":started.elapsed().as_millis()}),
    );
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn endpoint_does_not_expose_credentials_queries_or_fragments() {
        assert!(endpoint("https://user:password@example.com").is_err());
        assert!(endpoint("file:///private").is_err());
        assert_eq!(
            endpoint("https://example.com/old?secret=value#fragment")
                .unwrap()
                .as_str(),
            "wss://example.com/api/v1/voice"
        );
        assert_eq!(
            endpoint("http://[::1]:3000").unwrap().as_str(),
            "ws://[::1]:3000/api/v1/voice"
        );
    }

    #[test]
    fn arbitrary_request_id_headers_are_not_logged() {
        let connection = Connection::default();
        let mut headers = tokio_tungstenite::tungstenite::http::HeaderMap::new();
        headers.insert(
            thiscord_shared::REQUEST_ID_HEADER,
            "private-header-content".parse().unwrap(),
        );
        record_request_id(&connection, &headers);
        let details = connection.diagnostic_details(serde_json::json!({}));
        assert!(details["request_id"].is_null());
        assert!(!details.to_string().contains("private-header-content"));
    }

    #[test]
    fn alternate_family_is_tried_second_and_duplicates_are_removed() {
        let addresses: Vec<SocketAddr> = [
            "[::1]:1",
            "[::2]:1",
            "127.0.0.1:1",
            "[::1]:1",
            "127.0.0.2:1",
        ]
        .map(|s| s.parse().unwrap())
        .to_vec();
        let ordered = candidates(addresses.clone());
        assert_eq!(
            ordered,
            vec![addresses[0], addresses[2], addresses[1], addresses[4]]
        );
    }

    #[tokio::test]
    async fn reaches_second_address_when_first_refuses() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_address = closed.local_addr().unwrap();
        let open = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        drop(closed);
        let socket = timeout(
            Duration::from_secs(2),
            dial(
                vec![closed_address, open.local_addr().unwrap()],
                Duration::from_millis(5),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(socket.peer_addr().unwrap(), open.local_addr().unwrap());
    }

    #[tokio::test]
    async fn reports_handshake_timeout_after_tcp_connected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let error = connect_with_deadlines(
            &base,
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_millis(50),
            &Connection::default(),
        )
        .await
        .unwrap_err();
        server.abort();
        assert!(error.message.contains("TLS/WebSocket handshake"));
        assert!(error.message.contains("timed out"));
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn reports_upgrade_status_without_response_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 2048];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nx-request-id: 00000000-0000-0000-0000-000000000001\r\nContent-Length: 12\r\nConnection: close\r\n\r\nprivate-data").await.unwrap();
        });
        let connection = Connection::default();
        let error = connect_for(&base, &connection).await.unwrap_err();
        assert_eq!(
            connection.diagnostic_details(serde_json::json!({}))["request_id"],
            "00000000-0000-0000-0000-000000000001"
        );
        server.await.unwrap();
        assert!(error.message.contains("HTTP 403"));
        assert!(!error.message.contains("private-data"));
        assert!(!error.retryable);
    }

    #[test]
    fn transient_upgrade_statuses_retry_but_auth_and_bad_endpoints_do_not() {
        let url = endpoint("https://example.com").unwrap();
        for (status, retryable) in [
            (401, false),
            (403, false),
            (404, false),
            (429, true),
            (502, true),
            (503, true),
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body(Some(b"private-response".to_vec()))
                .unwrap();
            let error = handshake_error(Error::Http(Box::new(response)), &url);
            assert_eq!(error.retryable, retryable);
            assert!(!error.message.contains("private-response"));
        }
    }

    #[tokio::test]
    #[allow(
        clippy::result_large_err,
        reason = "The tungstenite handshake callback fixes the response error type"
    )]
    async fn native_upgrade_preserves_voice_path_and_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), thiscord_shared::voice::VOICE_PATH);
                    assert_eq!(request.headers()["origin"], "http://tauri.localhost");
                    assert!(!request.headers().contains_key("authorization"));
                    response.headers_mut().insert(thiscord_shared::REQUEST_ID_HEADER, "00000000-0000-0000-0000-000000000001".parse().unwrap());
                    Ok(response)
                },
            )
            .await
            .unwrap()
        });
        let connection = Connection::default();
        let _client = connect_for(&base, &connection).await.unwrap();
        assert_eq!(
            connection.diagnostic_details(serde_json::json!({}))["request_id"],
            "00000000-0000-0000-0000-000000000001"
        );
        let _server = server.await.unwrap();
    }
}
