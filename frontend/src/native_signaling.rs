//! Native voice signaling has a separate network path from the WebView.
//! Diagnose each stage without logging account tokens, HTTP bodies or SDP.
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{io, net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Error, client::IntoClientRequest, protocol::WebSocketConfig},
};

const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

fn handshake_error(error: Error, endpoint: &url::Url) -> String {
    match error {
        Error::Http(response) => format!(
            "Voice WebSocket rejected by {endpoint} (HTTP {}).",
            response.status().as_u16()
        ),
        Error::Tls(_) => format!(
            "Voice TLS verification or negotiation failed for {endpoint}. Check the Mac/PC clock, server certificate and any network TLS inspection."
        ),
        Error::Io(error) => format!(
            "Voice TLS/WebSocket connection to {endpoint} failed ({:?}).{}",
            error.kind(),
            network_hint()
        ),
        _ => format!("Invalid voice WebSocket handshake from {endpoint}."),
    }
}

pub async fn connect(base: &str) -> Result<Socket, String> {
    connect_with_deadlines(base, DNS_TIMEOUT, TCP_TIMEOUT, HANDSHAKE_TIMEOUT).await
}

async fn connect_with_deadlines(
    base: &str,
    dns_timeout: Duration,
    tcp_timeout: Duration,
    handshake_timeout: Duration,
) -> Result<Socket, String> {
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
                format!("Voice DNS lookup timed out for {host}. Check DNS and VPN settings.")
            })?
            .map_err(|_| {
                format!("Cannot resolve voice server {host}. Check DNS and VPN settings.")
            })?
            .collect(),
    };
    let stream = timeout(tcp_timeout, dial(addresses, Duration::from_millis(250)))
        .await
        .map_err(|_| {
            format!(
                "Voice TCP connection to {endpoint} timed out.{}",
                network_hint()
            )
        })?
        .map_err(|error| {
            format!(
                "Voice TCP connection to {endpoint} failed ({:?}).{}",
                error.kind(),
                network_hint()
            )
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
    let (socket, _) = timeout(
        handshake_timeout,
        tokio_tungstenite::client_async_tls_with_config(
            request,
            stream,
            Some(WebSocketConfig::default().max_message_size(Some(128 * 1024)).max_frame_size(Some(128 * 1024))),
            None,
        ),
    )
    .await
    .map_err(|_| format!("Voice TLS/WebSocket handshake with {endpoint} timed out. Check VPN, proxy and firewall settings."))?
    .map_err(|error| handshake_error(error, &endpoint))?;
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
        )
        .await
        .unwrap_err();
        server.abort();
        assert!(error.contains("TLS/WebSocket handshake"));
        assert!(error.contains("timed out"));
    }

    #[tokio::test]
    async fn reports_upgrade_status_without_response_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 2048];
            socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 12\r\nConnection: close\r\n\r\nprivate-data").await.unwrap();
        });
        let error = connect(&base).await.unwrap_err();
        server.await.unwrap();
        assert!(error.contains("HTTP 403"));
        assert!(!error.contains("private-data"));
    }

    #[tokio::test]
    async fn native_upgrade_preserves_voice_path_and_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), thiscord_shared::voice::VOICE_PATH);
                    assert_eq!(request.headers()["origin"], "http://tauri.localhost");
                    assert!(!request.headers().contains_key("authorization"));
                    Ok(response)
                },
            )
            .await
            .unwrap()
        });
        let _client = connect(&base).await.unwrap();
        let _server = server.await.unwrap();
    }
}
