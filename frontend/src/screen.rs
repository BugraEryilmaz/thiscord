//! Native H.264 video transport; independent of the real-time audio worker.
pub mod adaptation;
pub mod bridge;
pub mod cadence;
#[cfg(target_os = "windows")]
pub mod capture;
#[cfg(target_os = "windows")]
pub mod gpu;
#[cfg(target_os = "windows")]
pub mod hardware;
pub mod keyframe;
pub mod metrics;
pub mod pacing;
pub mod preview;
pub mod send;
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

use bytes::Bytes;
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

pub fn packetize(
    data: Vec<u8>,
    sequence: &mut u16,
    timestamp: u32,
) -> Result<Vec<rtp::Packet>, String> {
    use rtp::packetizer::Payloader;
    let payloads = rtp::codec::h264::H264Payloader::default()
        .payload(1100, &Bytes::from(data))
        .map_err(|_| "Cannot packetize screen frame")?;
    let count = payloads.len();
    Ok(payloads
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
        .collect())
}

pub use thiscord_shared::h264::{
    ParameterSetError, bounded_parameter_sets, validate_parameter_sets,
};

#[cfg(test)]
mod tests {
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
