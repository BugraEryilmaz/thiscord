//! Synthetic browser interop probe, explicitly run by a developer. No accounts,
//! screen capture, microphone, recording, STUN or TURN. Binds loopback only.
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use thiscord_frontend::screen::{packetize, preview::Preview};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18741").await?;
    println!("Synthetic screen preview probe: http://127.0.0.1:18741 (one viewer, 20 seconds)");
    let preview = loop {
        let (mut socket, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut block = [0; 4096];
        let (end, length) = loop {
            let count =
                tokio::time::timeout(Duration::from_secs(5), socket.read(&mut block)).await??;
            if count == 0 || request.len() > 70_000 {
                return Err("Invalid probe request".into());
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
        let (body, mime, session) = if request.starts_with(b"POST /offer ") {
            let offer: serde_json::Value = serde_json::from_slice(&request[end..end + length])?;
            let (preview, sdp) =
                Preview::answer(offer["sdp"].as_str().ok_or("Missing probe offer")?).await?;
            (
                serde_json::to_string(&serde_json::json!({"type":"answer","sdp":sdp}))?,
                "application/json",
                Some(preview),
            )
        } else {
            ("<!doctype html><title>Thiscord video probe</title><video id=video autoplay muted playsinline style='width:960px'></video>".into(),"text/html",None)
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await?;
        socket.write_all(body.as_bytes()).await?;
        socket.shutdown().await?;
        if let Some(preview) = session {
            break preview;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !preview.connected.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return Err("Browser WebRTC connection failed".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!("Browser connected to loopback video bridge");
    use openh264::{
        encoder::{Encoder, EncoderConfig, Profile, UsageType},
        formats::{RgbaSliceU8, YUVBuffer},
    };
    let (w, h) = (1280, 720);
    let mut encoder = Encoder::with_api_config(
        openh264::OpenH264API::from_source(),
        EncoderConfig::new()
            .profile(Profile::Baseline)
            .usage_type(UsageType::ScreenContentRealTime),
    )?;
    let mut rgba = vec![255; w * h * 4];
    let mut yuv = YUVBuffer::new(w, h);
    let mut sequence = 0;
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut count = 0;
    while started.elapsed() < Duration::from_secs(20) && preview.connected.load(Ordering::Acquire) {
        tick.tick().await;
        for row in rgba.chunks_mut(w * 4) {
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                pixel[0] = ((x + count * 8) % 256) as u8;
                pixel[1] = ((x / 4 + count * 3) % 256) as u8;
            }
        }
        yuv.read_rgba8(RgbaSliceU8::new(&rgba, (w, h)));
        if count % 60 == 0 {
            encoder.force_intra_frame();
        }
        let data = encoder.encode(&yuv)?.to_vec();
        for packet in packetize(
            data,
            &mut sequence,
            (started.elapsed().as_micros() * 90 / 1000) as u32,
        )? {
            preview.write(packet).await?;
        }
        count += 1;
    }
    println!(
        "Forwarded {count} synthetic frames in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
