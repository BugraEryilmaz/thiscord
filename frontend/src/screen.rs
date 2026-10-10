//! Native H.264 video transport; independent of the real-time audio worker.
pub mod adaptation;
pub mod cadence;
#[cfg(target_os = "windows")]
pub mod capture;
#[cfg(target_os = "windows")]
pub mod gpu;
#[cfg(target_os = "windows")]
pub mod hardware;
pub mod metrics;
pub mod pacing;
pub mod preview;
pub const SENDER_QUEUE_AGE: std::time::Duration = std::time::Duration::from_millis(200);
pub fn sender_queue_frames(fps: u32) -> usize {
    (fps.clamp(1, 60) as usize).div_ceil(5)
}
/// Reserve queue slots for hardware output that has not completed yet.
pub fn encoder_has_capacity(available: usize, pending: usize) -> bool {
    available > pending
}
pub struct Outgoing {
    pub captured_at: std::time::Instant,
    pub enqueued_at: std::time::Instant,
    pub packets: Vec<rtc::rtp::Packet>,
    pub keyframe: bool,
    pub budget: pacing::PendingBytes,
}

pub fn recovery_frame(data: &[u8]) -> bool {
    let mut kinds = [false; 32];
    for nal in openh264::nal_units(data) {
        if let Some(byte) = nal.strip_prefix(&[0, 0, 1]).unwrap_or(nal).first() {
            kinds[(byte & 31) as usize] = true;
        }
    }
    kinds[5] && kinds[7] && kinds[8]
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

/// Drop an entire damaged frame. Do not accumulate unbounded FU-A fragments or
/// decode a partial access unit after loss. The receive queue owns reordering
/// and recovery feedback; periodic IDRs are a compatibility fallback.
#[derive(Default)]
pub struct Assembler {
    timestamp: Option<u32>,
    next: Option<u16>,
    damaged: bool,
    fragment: bool,
    data: Vec<u8>,
}
impl Assembler {
    pub fn push(&mut self, packet: &rtp::Packet) -> Option<Vec<u8>> {
        if self.timestamp != Some(packet.header.timestamp) {
            self.timestamp = Some(packet.header.timestamp);
            self.damaged = false;
            self.fragment = false;
            self.data.clear();
        }
        if self
            .next
            .is_some_and(|n| n != packet.header.sequence_number)
        {
            self.damaged = true;
        }
        self.next = Some(packet.header.sequence_number.wrapping_add(1));
        let p = &packet.payload;
        if self.damaged || p.is_empty() || self.data.len() + p.len() + 16 > MAX_FRAME_BYTES {
            self.damaged = true;
            return None;
        }
        let valid = match p[0] & 31 {
            1..=23 if !self.fragment => {
                self.nal(p);
                true
            }
            24 if !self.fragment => {
                let mut offset = 1;
                let mut valid = true;
                while offset < p.len() {
                    if offset + 2 > p.len() {
                        valid = false;
                        break;
                    }
                    let size = u16::from_be_bytes([p[offset], p[offset + 1]]) as usize;
                    offset += 2;
                    if size == 0
                        || offset + size > p.len()
                        || self.data.len() + size + 4 > MAX_FRAME_BYTES
                    {
                        valid = false;
                        break;
                    }
                    self.nal(&p[offset..offset + size]);
                    offset += size;
                }
                valid
            }
            28 if p.len() > 2 => {
                let start = p[1] & 128 != 0;
                let end = p[1] & 64 != 0;
                if start == self.fragment || (start && end) || p[1] & 31 == 0 {
                    false
                } else {
                    if start {
                        self.data
                            .extend_from_slice(&[0, 0, 0, 1, (p[0] & 0xe0) | (p[1] & 31)]);
                    }
                    self.data.extend_from_slice(&p[2..]);
                    self.fragment = !end;
                    true
                }
            }
            _ => false,
        };
        if !valid {
            self.damaged = true;
            return None;
        }
        if packet.header.marker && !self.fragment {
            return Some(std::mem::take(&mut self.data));
        }
        None
    }
    fn nal(&mut self, data: &[u8]) {
        self.data.extend_from_slice(&[0, 0, 0, 1]);
        self.data.extend_from_slice(data);
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

/// Restrict remote SPS dimensions/reference counts before the video decoder can
/// allocate its picture buffers. Only progressive baseline is negotiated here.
pub fn bounded_parameter_sets(data: &[u8]) -> bool {
    openh264::nal_units(data).all(|nal| {
        let nal = nal.strip_prefix(&[0, 0, 1]).unwrap_or(nal);
        if nal.first().is_none_or(|b| b & 31 != 7) {
            return true;
        }
        bounded_sps(nal).unwrap_or(false)
    })
}
fn bounded_sps(nal: &[u8]) -> Option<bool> {
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal.get(1..)? {
        if zeros == 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    if rbsp.first() != Some(&66) {
        return Some(false);
    }
    let mut bits = Bits {
        bytes: &rbsp,
        offset: 24,
    };
    if bits.ue()? > 31 || bits.ue()? > 12 {
        return Some(false);
    }
    match bits.ue()? {
        0 => {
            if bits.ue()? > 12 {
                return Some(false);
            }
        }
        2 => {}
        _ => return Some(false),
    }
    if bits.ue()? > 4 {
        return Some(false);
    }
    bits.bit()?;
    let width = bits.ue()?.checked_add(1)?.checked_mul(16)?;
    let height = bits.ue()?.checked_add(1)?.checked_mul(16)?;
    let progressive = bits.bit()?;
    Some(progressive && width <= MAX_WIDTH && height <= MAX_HEIGHT.div_ceil(16) * 16)
}
struct Bits<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Bits<'_> {
    fn bit(&mut self) -> Option<bool> {
        let b = self.bytes.get(self.offset / 8)? & (128 >> (self.offset % 8)) != 0;
        self.offset += 1;
        Some(b)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > 20 {
                return None;
            }
        }
        let mut value = 1_u32;
        for _ in 0..zeros {
            value = value * 2 + u32::from(self.bit()?);
        }
        Some(value - 1)
    }
}

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
    #[test]
    fn sps_rejects_dimensions_beyond_receiver_budget() {
        fn sps(width: u32, height: u32) -> Vec<u8> {
            let mut bits = String::new();
            for value in [0, 0, 2, 1] {
                let code = format!("{:b}", value + 1);
                bits.push_str(&"0".repeat(code.len() - 1));
                bits.push_str(&code);
            }
            bits.push('0');
            for value in [width / 16 - 1, height / 16 - 1] {
                let code = format!("{:b}", value + 1);
                bits.push_str(&"0".repeat(code.len() - 1));
                bits.push_str(&code);
            }
            bits.push('1');
            while !bits.len().is_multiple_of(8) {
                bits.push('0');
            }
            let mut nal = vec![0x67, 66, 0xe0, 52];
            for byte in bits.as_bytes().chunks(8) {
                nal.push(byte.iter().fold(0, |v, b| v * 2 + (b - b'0')));
            }
            nal
        }
        assert_eq!(bounded_sps(&sps(1920, 1088)), Some(true));
        assert_eq!(bounded_sps(&sps(3840, 2160)), Some(true));
        assert_eq!(bounded_sps(&sps(3856, 2160)), Some(false));
        assert_eq!(bounded_sps(&sps(3840, 2176)), Some(false));
    }
}

/// Complete-frame backlog. Lost dependencies discard queued work and require a
/// fresh SPS/PPS + IDR, rather than decoding a growing FIFO of stale packets.
pub mod receive;
