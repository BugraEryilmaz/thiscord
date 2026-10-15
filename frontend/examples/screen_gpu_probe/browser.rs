use std::sync::Arc;
use std::time::Duration;
use thiscord_frontend::screen::preview::Preview;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn connect() -> Result<Arc<Preview>, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18741").await?;
    println!(
        "Open http://127.0.0.1:18741 and negotiate the diagnostic video peer. No account is required."
    );
    'requests: loop {
        let (mut socket, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut block = [0u8; 4096];
        let (end, length) = loop {
            let count =
                match tokio::time::timeout(Duration::from_secs(2), socket.read(&mut block)).await {
                    Ok(Ok(count)) => count,
                    // Browsers may open speculative sockets without sending a request.
                    _ => continue 'requests,
                };
            if count == 0 || request.len() > 70_000 {
                continue 'requests;
            }
            request.extend_from_slice(&block[..count]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = std::str::from_utf8(&request[..end])?;
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if length > 64 * 1024 {
                    return Err("Probe offer too large".into());
                }
                break (end + 4, length);
            }
        };
        while request.len() < end + length {
            let count =
                tokio::time::timeout(Duration::from_secs(5), socket.read(&mut block)).await??;
            if count == 0 {
                return Err("Incomplete probe request".into());
            }
            request.extend_from_slice(&block[..count]);
        }
        let (body, mime, preview) = if request.starts_with(b"POST /offer ") {
            let offer: serde_json::Value = serde_json::from_slice(&request[end..end + length])?;
            let (preview, sdp) =
                Preview::answer(offer["sdp"].as_str().ok_or("Missing offer")?).await?;
            (
                serde_json::to_string(&serde_json::json!({"type":"answer", "sdp":sdp}))?,
                "application/json",
                Some(Arc::new(preview)),
            )
        } else {
            ("<!doctype html><meta charset=utf-8><title>Thiscord 4K pipeline probe</title><body style='margin:0;background:#111;color:#eee;font:16px sans-serif'><p>3840×2160 live synthetic Windows capture → GPU H.264 → paced WebRTC → receive queue → browser WebRTC</p><video id='video' autoplay muted playsinline style='width:100%;max-height:90vh;object-fit:contain'></video></body>".into(), "text/html", None)
        };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
        socket.write_all(body.as_bytes()).await?;
        socket.shutdown().await?;
        if let Some(preview) = preview {
            return Ok(preview);
        }
    }
}
