//! Native H.264 video transport; independent of the real-time audio worker.
pub mod adaptation;
pub mod bridge;
pub mod cadence;
#[cfg(target_os = "windows")]
pub mod capture;
pub mod encode;
#[cfg(target_os = "windows")]
pub mod gpu;
#[cfg(target_os = "windows")]
pub mod hardware;
/// xdg-desktop-portal ScreenCast + PipeWire capture, GStreamer GPU encoders.
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux as capture;
/// ScreenCaptureKit capture and VideoToolbox hardware encoding.
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as capture;
pub mod keyframe;
pub mod metrics;
pub mod pacing;
pub mod preview;
pub mod priority;
pub mod send;
pub mod worker;
/// UDP binds for the in-process WebView video bridges (screen preview, camera).
/// Once media permission enables adapter enumeration, WebView binds its host
/// socket to an interface address. Windows refuses to send from such a socket
/// to 127.0.0.1 (WSAEADDRNOTAVAIL) or to another local address, so the answer
/// must offer the WebView's own address. The wildcard expands to one socket per
/// usable IPv4 interface. The ports are LAN-reachable, but ICE credentials and
/// the DTLS fingerprint exchanged in-process still pin the session to the WebView.
pub fn local_bridge_udp_addrs() -> Vec<String> {
    vec!["127.0.0.1:0".to_owned(), "0.0.0.0:0".to_owned()]
}
pub const SENDER_QUEUE_AGE: std::time::Duration = std::time::Duration::from_millis(200);
pub fn sender_queue_frames(fps: u32) -> usize {
    (fps.clamp(1, 60) as usize).div_ceil(5)
}
/// Reserve queue slots for hardware output that has not completed yet.
pub fn encoder_has_capacity(available: usize, pending: usize) -> bool {
    available > pending
}
pub struct Outgoing {
    pub bitrate: u32,
    pub captured_at: std::time::Instant,
    pub enqueued_at: std::time::Instant,
    pub packets: Vec<rtc::rtp::Packet>,
    /// The encoded Annex B frame the packets were cut from. Single-NAL packets
    /// already share this buffer; the sender hands it to the own preview whole
    /// (a reference count, not a copy) once the frame is fully paced.
    pub frame: Bytes,
    pub keyframe: bool,
    pub budget: pacing::PendingBytes,
}

/// One optional low encoder shares the capture surface with the primary path.
pub struct LayerOutput {
    pub metrics: metrics::Metrics,
    pub tx: tokio::sync::mpsc::Sender<Outgoing>,
    pub demand: std::sync::Arc<adaptation::Demand>,
    pub force_keyframe: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

use bytes::{BufMut, Bytes, BytesMut};
use rtc::{
    media_stream::MediaStreamTrack,
    rtp,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
    },
};
use std::sync::Arc;
pub use thiscord_shared::h264::recovery_frame;
#[cfg(test)]
use thiscord_shared::screen::*;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;

pub fn track(ssrc: u32) -> Arc<TrackLocalStaticRTP> {
    Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("screen-{ssrc}"),
        format!("screen-{ssrc}"),
        format!("screen-{ssrc}"),
        RtpCodecKind::Video,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec: RTCRtpCodec {
                mime_type: "video/H264".into(),
                clock_rate: 90_000,
                sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                ..Default::default()
            },
            ..Default::default()
        }],
    )))
}

/// Native RTP adapter for the portable bounded H.264 assembler.
#[derive(Default)]
pub struct Assembler(thiscord_shared::h264::Assembler);
impl Assembler {
    pub fn push(&mut self, packet: &rtp::Packet) -> Option<Vec<u8>> {
        self.0.push(
            packet.header.timestamp,
            packet.header.sequence_number,
            packet.header.marker,
            &packet.payload,
        )
    }
}

/// RTP payload size limit for screen and camera video.
const VIDEO_MTU: usize = 1100;

/// RFC 6184 packetization mode 1 payloads with the same output as webrtc-rs's
/// `H264Payloader` (AUD/filler dropped, SPS+PPS sent as one STAP-A before the
/// next NAL unit, FU-A above the MTU), but app-owned: NAL units that fit are
/// zero-copy slices of the frame, and every STAP-A/FU-A payload is cut from one
/// buffer reserved per frame instead of one allocation per fragment.
/// NAL units as zero-copy slices, and whether the frame had Annex B start
/// codes (without any, the whole frame is sent as one unit).
fn nal_slices(frame: &Bytes) -> (Vec<Bytes>, bool) {
    let nals: Vec<Bytes> = thiscord_shared::h264::nal_units(frame)
        .filter(|nal| !nal.is_empty())
        .map(|nal| frame.slice_ref(nal))
        .collect();
    if nals.is_empty() && thiscord_shared::h264::nal_units(frame).next().is_none() {
        (vec![frame.clone()], false)
    } else {
        (nals, true)
    }
}
#[cfg(test)]
fn payloads(frame: Bytes) -> Vec<Bytes> {
    payloads_from(nal_slices(&frame).0)
}
fn payloads_from(nals: Vec<Bytes>) -> Vec<Bytes> {
    const FUA: u8 = 28;
    let fragments = nals
        .iter()
        .filter(|n| n.len() > VIDEO_MTU)
        .map(|n| n.len() + 2 * (n.len() / (VIDEO_MTU - 2) + 1))
        .sum::<usize>();
    let mut buffer = BytesMut::with_capacity(fragments + 2 * VIDEO_MTU);
    let mut out = Vec::with_capacity(nals.len() + fragments / VIDEO_MTU + 1);
    let (mut sps, mut pps): (Option<Bytes>, Option<Bytes>) = (None, None);
    for nal in nals {
        let kind = nal[0] & 0x1f;
        match kind {
            9 | 12 => continue,
            7 => {
                sps = Some(nal);
                continue;
            }
            8 => {
                pps = Some(nal);
                continue;
            }
            _ => {}
        }
        // Like the webrtc-rs payloader: a lone SPS or PPS waits for its pair;
        // a pair too large for one STAP-A is dropped.
        if let (Some(s), Some(p)) = (&sps, &pps)
            && 5 + s.len() + p.len() <= VIDEO_MTU
        {
            buffer.put_u8(0x78);
            buffer.put_u16(s.len() as u16);
            buffer.put_slice(s);
            buffer.put_u16(p.len() as u16);
            buffer.put_slice(p);
            out.push(buffer.split().freeze());
        }
        if sps.is_some() && pps.is_some() {
            (sps, pps) = (None, None);
        }
        if nal.len() <= VIDEO_MTU {
            out.push(nal);
            continue;
        }
        let (indicator, kind) = (FUA | (nal[0] & 0x60), kind);
        let mut chunks = nal[1..].chunks(VIDEO_MTU - 2).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let mut header = kind;
            if first {
                header |= 0x80;
            } else if chunks.peek().is_none() {
                header |= 0x40;
            }
            first = false;
            buffer.put_u8(indicator);
            buffer.put_u8(header);
            buffer.put_slice(chunk);
            out.push(buffer.split().freeze());
        }
    }
    out
}

/// Awaits `future` for at most `limit`, but builds no timer when it completes
/// on its first poll: an RTP write normally only enqueues for the peer's
/// driver task, so per-packet writes skip the timeout's clock read and entry.
pub async fn ready_or_timeout<F: std::future::Future>(
    future: F,
    limit: std::time::Duration,
) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    if let std::task::Poll::Ready(output) =
        std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
    {
        return Some(output);
    }
    tokio::time::timeout(limit, future).await.ok()
}

pub fn packetize(
    data: impl Into<Bytes>,
    sequence: &mut u16,
    timestamp: u32,
) -> Result<Vec<rtp::Packet>, String> {
    let data = data.into();
    if data.is_empty() {
        return Ok(vec![]);
    }
    Ok(packets(
        payloads_from(nal_slices(&data).0),
        sequence,
        timestamp,
    ))
}

/// [`packetize`] for frames received from other publishers: every SPS is
/// validated (as [`thiscord_shared::h264::parameter_set_profile`] would) during
/// the same NAL split the packetizer uses, so the relay walks a frame once.
/// Returns the last SPS's profile; nothing is packetized when one is rejected.
pub fn packetize_validated(
    data: impl Into<Bytes>,
    sequence: &mut u16,
    timestamp: u32,
    allow_high: bool,
) -> Result<(Vec<rtp::Packet>, Option<thiscord_shared::h264::Profile>), ParameterSetError> {
    let (nals, annex_b) = nal_slices(&data.into());
    let mut profile = None;
    for nal in nals.iter().filter(|n| annex_b && n[0] & 0x1f == 7) {
        profile = Some(thiscord_shared::h264::sps_profile(nal, allow_high)?);
    }
    if nals.is_empty() {
        return Ok((vec![], profile));
    }
    Ok((packets(payloads_from(nals), sequence, timestamp), profile))
}

fn packets(payloads: Vec<Bytes>, sequence: &mut u16, timestamp: u32) -> Vec<rtp::Packet> {
    let count = payloads.len();
    payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            *sequence = sequence.wrapping_add(1);
            rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: thiscord_shared::voice::MediaKind::ScreenVideo.payload_type(),
                    ssrc: thiscord_shared::voice::MediaKind::ScreenVideo.publisher_ssrc(),
                    sequence_number: *sequence,
                    timestamp,
                    marker: i + 1 == count,
                    ..Default::default()
                },
                payload,
            }
        })
        .collect()
}

pub use thiscord_shared::h264::{
    ParameterSetError, bounded_parameter_sets, validate_parameter_sets,
};

#[cfg(test)]
mod tests {
    /// The app-owned packetizer must emit exactly what the webrtc-rs payloader
    /// it replaced emitted, including STAP-A pairing and FU-A boundaries.
    #[test]
    fn packetizer_matches_the_webrtc_rs_payloader() {
        use rtc::rtp::packetizer::Payloader;
        let mut state = 0x9e37_79b9_u32;
        let mut byte = |zero_bias: bool| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if zero_bias && state.is_multiple_of(5) {
                0
            } else {
                (state >> 8) as u8 | 2
            }
        };
        let mut cases: Vec<Vec<u8>> = vec![
            vec![0x65; 50],                           // no start code: one NAL unit
            vec![0, 0, 1, 0x09, 0xf0],                // AUD only
            vec![0, 0, 0, 1, 0x67, 1, 2, 3],          // SPS without PPS
            vec![0, 0, 1, 0x68, 1, 0, 0, 1, 0x41, 9], // lone PPS before a slice
        ];
        for size in [
            10, 1_099, 1_100, 1_101, 2_197, 2_198, 2_199, 25_000, 300_000,
        ] {
            for kinds in [
                [0x09, 0x67, 0x68, 0x65],
                [0x09, 0x41, 0x41, 0x41],
                [0x67, 0x68, 0x06, 0x65],
            ] {
                let mut frame = Vec::new();
                for (i, kind) in kinds.into_iter().enumerate() {
                    frame.extend_from_slice(if i % 2 == 0 {
                        &[0, 0, 0, 1][..]
                    } else {
                        &[0, 0, 1]
                    });
                    frame.push(kind);
                    let length = match kind {
                        0x67 | 0x68 | 0x09 | 0x06 => 6,
                        _ => size,
                    };
                    frame.extend((0..length).map(|_| byte(true)));
                    // Trailing zeros before the next start code belong to it.
                    if frame.last() == Some(&0) {
                        frame.push(5);
                    }
                }
                cases.push(frame);
            }
        }
        // A large SPS/PPS pair that cannot fit one STAP-A is dropped.
        let mut big = vec![0, 0, 1, 0x67];
        big.extend(std::iter::repeat_n(7, 600));
        big.extend([0, 0, 1, 0x68]);
        big.extend(std::iter::repeat_n(8, 600));
        big.extend([0, 0, 1, 0x65, 4, 4]);
        cases.push(big);
        // An SPS after the slice must still be validated by the relay.
        cases.push(vec![0, 0, 1, 0x65, 9, 9, 0, 0, 1, 0x67, 66]);
        for frame in cases {
            let expected = rtc::rtp::codec::h264::H264Payloader::default()
                .payload(1100, &bytes::Bytes::from(frame.clone()))
                .unwrap();
            // The relay's single walk agrees with the full parameter-set check.
            let validated = super::packetize_validated(frame.clone(), &mut 0, 0, false);
            match thiscord_shared::h264::parameter_set_profile(&frame, false) {
                Ok(profile) => {
                    let (packets, found) = validated.unwrap();
                    assert_eq!(found, profile);
                    let payloads: Vec<_> = packets.into_iter().map(|p| p.payload).collect();
                    assert_eq!(payloads, expected);
                }
                Err(error) => assert_eq!(validated.err(), Some(error)),
            }
            assert_eq!(super::payloads(bytes::Bytes::from(frame)), expected);
        }
    }

    #[test]
    fn sender_admission_reserves_space_for_unfinished_encodes() {
        for fps in [30, 60] {
            let slots = super::sender_queue_frames(fps);
            assert_eq!(slots, fps as usize / 5);
            let mut available = slots;
            let mut pending = 0;
            while super::encoder_has_capacity(available, pending) {
                pending += 1;
            }
            assert_eq!(pending, slots);
            // Every admitted encode can complete even while the sender stalls.
            while pending > 0 {
                available -= 1;
                pending -= 1;
                assert!(!super::encoder_has_capacity(available, pending));
            }
            available += 1; // Sending a queued frame permits one new encode.
            assert!(super::encoder_has_capacity(available, pending));
            pending += 1;
            assert!(!super::encoder_has_capacity(available, pending));
        }
    }
    use super::*;
    #[test]
    fn fragmentation_loss_bounds_and_recovery() {
        let mut seq = u16::MAX - 3;
        let mut data = vec![0, 0, 0, 1, 0x65];
        data.extend(vec![13; 6000]);
        let packets = packetize(data.clone(), &mut seq, 9000).unwrap();
        let mut assembler = Assembler::default();
        let result = packets
            .iter()
            .filter_map(|p| assembler.push(p))
            .last()
            .unwrap();
        assert_eq!(result, data);
        let mut assembler = Assembler::default();
        for (i, packet) in packets.iter().enumerate() {
            if i != 2 {
                assert!(assembler.push(packet).is_none());
            }
        }
        let recovered = packetize(data.clone(), &mut seq, 18000).unwrap();
        assert_eq!(
            recovered.iter().filter_map(|p| assembler.push(p)).last(),
            Some(data)
        );
        let mut malformed = recovered[0].clone();
        malformed.payload = Bytes::from_static(&[24, 0]);
        assert!(Assembler::default().push(&malformed).is_none());
    }

    #[test]
    fn encoded_screen_survives_rtp_and_decodes_with_bounded_dimensions() {
        use openh264::{
            encoder::{Encoder, EncoderConfig, Profile, UsageType},
            formats::{RgbSliceU8, YUVBuffer, YUVSource},
        };
        let rgb = vec![120; 320 * 180 * 3];
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (320, 180)));
        let mut encoder = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            EncoderConfig::new()
                .profile(Profile::Baseline)
                .usage_type(UsageType::ScreenContentRealTime),
        )
        .unwrap();
        let frame = encoder.encode(&yuv).unwrap().to_vec();
        assert!(bounded_parameter_sets(&frame));
        let mut assembler = Assembler::default();
        let mut sequence = 0;
        let packets = packetize(frame, &mut sequence, 9000).unwrap();
        let frame = packets
            .iter()
            .filter_map(|p| assembler.push(p))
            .last()
            .unwrap();
        assert!(bounded_parameter_sets(&frame));
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let decoded = decoder.decode(&frame).unwrap().unwrap();
        assert_eq!(decoded.dimensions(), (320, 180));
        let mut output = vec![0; 320 * 180 * 3];
        decoded.write_rgb8(&mut output);
        assert!(output.iter().all(|v| (115..=125).contains(v)));
        assert!(!bounded_parameter_sets(&[0, 0, 1, 0x67, 66]));
        assert!(!bounded_parameter_sets(&[0, 0, 1, 0x67, 100, 0, 31, 255]));
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;
    use openh264::{
        encoder::{BitRate, Encoder, EncoderConfig, FrameRate, Level, Profile, UsageType},
        formats::{RgbaSliceU8, YUVBuffer, YUVSource},
    };
    #[test]
    fn higher_resolutions_round_trip_through_rtp_and_decoder() {
        for height in [1080, 1440, 2160] {
            let quality = Quality { height, fps: 60 };
            let (w, h) = (quality.width() as usize, height as usize);
            let mut rgb = vec![0; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    rgb[i] = (x % 256) as u8;
                    rgb[i + 1] = (y % 256) as u8;
                    rgb[i + 2] = 120;
                }
            }
            let mut encoder = Encoder::with_api_config(
                openh264::OpenH264API::from_source(),
                EncoderConfig::new()
                    .profile(Profile::Baseline)
                    .level(Level::Level_5_2)
                    .usage_type(UsageType::ScreenContentRealTime)
                    .max_frame_rate(FrameRate::from_hz(quality.fps as f32))
                    .bitrate(BitRate::from_bps(quality.bitrate())),
            )
            .unwrap();
            let mut yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(&rgb, (w, h)));
            yuv.read_rgba8(RgbaSliceU8::new(&rgb, (w, h)));
            let frame = encoder.encode(&yuv).unwrap().to_vec();
            assert!(!frame.is_empty() && frame.len() <= MAX_FRAME_BYTES);
            assert!(bounded_parameter_sets(&frame));
            let (sender, inbox) = receive::Inbox::channel();
            let mut sequence = 0;
            for mut packet in packetize(frame, &mut sequence, 9000).unwrap() {
                packet.header.csrc = vec![1];
                sender.push(packet, std::time::Instant::now());
            }
            drop(sender);
            let frame = inbox
                .next()
                .expect("encoded keyframe must resynchronize the receiver");
            assert!(frame.reset);
            let frame = frame.data;
            let mut decoder = openh264::decoder::Decoder::new().unwrap();
            let decoded = decoder.decode(&frame).unwrap().unwrap();
            assert_eq!(decoded.dimensions(), (w, h));
        }
    }
}

/// Complete-frame backlog. Lost dependencies discard queued work and require a
/// fresh SPS/PPS + IDR, rather than decoding a growing FIFO of stale packets.
pub mod receive;
