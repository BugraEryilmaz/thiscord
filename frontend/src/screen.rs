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
pub mod send;
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

// Preserve the previous worst-case reference-picture budget (four 4K frames),
// while allowing smaller camera frames to use H.264's limit of 16 references.
const MAX_REFERENCE_PIXELS: u64 = MAX_WIDTH as u64 * (MAX_HEIGHT.div_ceil(16) * 16) as u64 * 4;

/// Restrict remote SPS dimensions/reference counts before the video decoder can
/// allocate its picture buffers. Only progressive baseline is negotiated here.
pub fn bounded_parameter_sets(data: &[u8]) -> bool {
    validate_parameter_sets(data).is_ok()
}

/// Safe diagnostic metadata only: never include encoded bytes or SDP.
#[derive(Debug, PartialEq, Eq)]
pub enum ParameterSetError {
    Malformed,
    Profile {
        id: u8,
        constraints: u8,
        level: u8,
    },
    SpsId(u32),
    FrameNumberBits(u32),
    PictureOrderBits(u32),
    PictureOrder(u32),
    PictureOrderCycle(u32),
    References(u32),
    ReferenceBudget {
        references: u32,
        width: u32,
        height: u32,
    },
    Dimensions {
        width: u32,
        height: u32,
    },
    Interlaced,
}
impl std::fmt::Display for ParameterSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "malformed or truncated H.264 SPS"),
            Self::Profile {
                id,
                constraints,
                level,
            } => write!(
                f,
                "unsupported H.264 profile-level-id={id:02x}{constraints:02x}{level:02x}; expected baseline"
            ),
            Self::SpsId(value) => write!(f, "H.264 SPS id {value} exceeds 31"),
            Self::FrameNumberBits(value) => {
                write!(f, "H.264 frame-number bit offset {value} exceeds 12")
            }
            Self::PictureOrderBits(value) => {
                write!(f, "H.264 picture-order bit offset {value} exceeds 12")
            }
            Self::PictureOrder(value) => write!(f, "unsupported H.264 picture-order type {value}"),
            Self::PictureOrderCycle(value) => {
                write!(f, "H.264 picture-order cycle {value} exceeds 255")
            }
            Self::References(value) => {
                write!(f, "H.264 reference frames {value} exceed codec limit 16")
            }
            Self::ReferenceBudget {
                references,
                width,
                height,
            } => write!(
                f,
                "H.264 {references} reference frames at {width}x{height} exceed reference-picture budget {MAX_REFERENCE_PIXELS} pixels"
            ),
            Self::Dimensions { width, height } => write!(
                f,
                "H.264 coded dimensions {width}x{height} exceed {MAX_WIDTH}x{MAX_HEIGHT}"
            ),
            Self::Interlaced => write!(
                f,
                "interlaced H.264 is unsupported; expected progressive video"
            ),
        }
    }
}

pub fn validate_parameter_sets(data: &[u8]) -> Result<(), ParameterSetError> {
    for nal in openh264::nal_units(data) {
        let nal = nal.strip_prefix(&[0, 0, 1]).unwrap_or(nal);
        if nal.first().is_none_or(|b| b & 31 != 7) {
            continue;
        }
        inspect_sps(nal).ok_or(ParameterSetError::Malformed)??;
    }
    Ok(())
}
#[cfg(test)]
fn bounded_sps(nal: &[u8]) -> Option<bool> {
    inspect_sps(nal).map(|result| result.is_ok())
}
fn inspect_sps(nal: &[u8]) -> Option<Result<(), ParameterSetError>> {
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
    // Constrained baseline has equivalent profile encodings (RFC 6184,
    // table 5). Hardware encoders need not use profile_idc 66.
    let id = *rbsp.first()?;
    let constraints = *rbsp.get(1)?;
    let level = *rbsp.get(2)?;
    let baseline = match id {
        66 => true,
        77 => constraints & 0x8f == 0x80,
        88 => constraints & 0xcf == 0xc0,
        _ => false,
    };
    if !baseline {
        return Some(Err(ParameterSetError::Profile {
            id,
            constraints,
            level,
        }));
    }
    let mut bits = Bits {
        bytes: &rbsp,
        offset: 24,
    };
    let sps_id = bits.ue()?;
    if sps_id > 31 {
        return Some(Err(ParameterSetError::SpsId(sps_id)));
    }
    let frame_bits = bits.ue()?;
    if frame_bits > 12 {
        return Some(Err(ParameterSetError::FrameNumberBits(frame_bits)));
    }
    match bits.ue()? {
        0 => {
            let order_bits = bits.ue()?;
            if order_bits > 12 {
                return Some(Err(ParameterSetError::PictureOrderBits(order_bits)));
            }
        }
        1 => {
            // Baseline also permits explicit picture-order offsets. They do
            // not change the dimension/reference budget, but must be consumed
            // before reading those fields. Signed Exp-Golomb has the same bit
            // length as unsigned, and these values are only skipped here.
            bits.bit()?;
            bits.ue()?;
            bits.ue()?;
            let cycle = bits.ue()?;
            if cycle > 255 {
                return Some(Err(ParameterSetError::PictureOrderCycle(cycle)));
            }
            for _ in 0..cycle {
                bits.ue()?;
            }
        }
        2 => {}
        value => return Some(Err(ParameterSetError::PictureOrder(value))),
    }
    let references = bits.ue()?;
    if references > 16 {
        return Some(Err(ParameterSetError::References(references)));
    }
    bits.bit()?;
    let width = bits.ue()?.checked_add(1)?.checked_mul(16)?;
    let height = bits.ue()?.checked_add(1)?.checked_mul(16)?;
    let progressive = bits.bit()?;
    Some(if !progressive {
        Err(ParameterSetError::Interlaced)
    } else if width > MAX_WIDTH || height > MAX_HEIGHT.div_ceil(16) * 16 {
        Err(ParameterSetError::Dimensions { width, height })
    } else if u64::from(width) * u64::from(height) * u64::from(references) > MAX_REFERENCE_PIXELS {
        Err(ParameterSetError::ReferenceBudget {
            references,
            width,
            height,
        })
    } else {
        Ok(())
    })
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
    fn baseline_camera_picture_order_and_profiles_preserve_decoder_bounds() {
        fn ue(bits: &mut String, value: u32) {
            let code = format!("{:b}", value + 1);
            bits.push_str(&"0".repeat(code.len() - 1));
            bits.push_str(&code);
        }
        fn sps(profile: u8, flags: u8, order: u32, cycle: u32, refs: u32, width: u32) -> Vec<u8> {
            let mut bits = String::new();
            ue(&mut bits, 0); // SPS id
            ue(&mut bits, 0); // log2_max_frame_num_minus4
            ue(&mut bits, order);
            match order {
                0 => ue(&mut bits, 0),
                1 => {
                    bits.push('1'); // delta_pic_order_always_zero_flag
                    ue(&mut bits, 1); // signed +1
                    ue(&mut bits, 2); // signed -1
                    ue(&mut bits, cycle);
                    for i in 0..cycle {
                        ue(&mut bits, i % 3);
                    }
                }
                _ => {}
            }
            ue(&mut bits, refs);
            bits.push('0');
            ue(&mut bits, width / 16 - 1);
            ue(&mut bits, 44); // 720px
            bits.push_str("11001"); // progressive, inference, no crop/VUI, trailing stop
            while !bits.len().is_multiple_of(8) {
                bits.push('0');
            }
            let mut nal = vec![0x67, profile, flags, 31];
            let mut zeros = 0;
            for byte in bits.as_bytes().chunks(8) {
                let byte = byte.iter().fold(0, |v, b| v * 2 + (b - b'0'));
                if zeros == 2 && byte <= 3 {
                    nal.push(3);
                    zeros = 0;
                }
                nal.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            nal
        }
        for (profile, flags) in [(66, 0xe0), (77, 0x80), (88, 0xc0)] {
            for order in 0..=2 {
                for cycle in [0, 2, 255] {
                    // Mac camera diagnostics reported 5, 8 and 9 references.
                    for refs in [0, 4, 5, 8, 9, 16] {
                        let nal = sps(profile, flags, order, cycle, refs, 1280);
                        assert_eq!(super::bounded_sps(&nal), Some(true));
                        let frame = [vec![0, 0, 0, 1], nal].concat();
                        assert!(super::bounded_parameter_sets(&frame));
                    }
                }
                assert_eq!(
                    super::bounded_sps(&sps(profile, flags, order, 2, 17, 1280)),
                    Some(false)
                );
                assert_eq!(
                    super::bounded_sps(&sps(profile, flags, order, 2, 4, 4096)),
                    Some(false)
                );
            }
        }
        assert_eq!(
            super::bounded_sps(&sps(66, 0xe0, 1, 256, 4, 1280)),
            Some(false)
        );
        assert_eq!(
            super::bounded_sps(&sps(66, 0xe0, 3, 0, 4, 1280)),
            Some(false)
        );
        for (profile, flags) in [(77, 0), (88, 0), (100, 0)] {
            assert_eq!(
                super::bounded_sps(&sps(profile, flags, 1, 2, 4, 1280)),
                Some(false)
            );
        }
        let nal = sps(66, 0xe0, 1, 2, 4, 1280);
        for end in 0..nal.len() - 1 {
            assert_ne!(super::bounded_sps(&nal[..end]), Some(true));
        }
        use super::{ParameterSetError, validate_parameter_sets};
        let check = |nal: Vec<u8>| validate_parameter_sets(&[vec![0, 0, 0, 1], nal].concat());
        assert_eq!(
            check(sps(100, 0x0c, 2, 0, 1, 1280)),
            Err(ParameterSetError::Profile {
                id: 100,
                constraints: 0x0c,
                level: 31
            }),
        );
        assert_eq!(
            check(sps(66, 0xe0, 2, 0, 17, 1280)),
            Err(ParameterSetError::References(17))
        );
        assert_eq!(
            check(sps(66, 0xe0, 2, 0, 1, 4096)),
            Err(ParameterSetError::Dimensions {
                width: 4096,
                height: 720
            })
        );
        assert_eq!(
            check(sps(66, 0xe0, 1, 256, 1, 1280)),
            Err(ParameterSetError::PictureOrderCycle(256))
        );
        assert_eq!(check(vec![0x67, 66]), Err(ParameterSetError::Malformed));
        assert_eq!(
            ParameterSetError::Profile {
                id: 100,
                constraints: 0x0c,
                level: 31
            }
            .to_string(),
            "unsupported H.264 profile-level-id=640c1f; expected baseline",
        );
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
    #[test]
    fn sps_enforces_dimensions_and_reference_memory_budget() {
        fn sps(width: u32, height: u32, references: u32) -> Vec<u8> {
            let mut bits = String::new();
            for value in [0, 0, 2, references] {
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
        assert_eq!(bounded_sps(&sps(1920, 1088, 1)), Some(true));
        assert_eq!(bounded_sps(&sps(3840, 2160, 4)), Some(true));
        assert_eq!(bounded_sps(&sps(3856, 2160, 1)), Some(false));
        assert_eq!(bounded_sps(&sps(3840, 2176, 1)), Some(false));
        // At 720p all 16 reference pictures fit inside the previous 4K budget.
        assert_eq!(bounded_sps(&sps(1280, 720, 16)), Some(true));
        assert_eq!(bounded_sps(&sps(1280, 720, 17)), Some(false));
        assert_eq!(bounded_sps(&sps(3840, 2160, 5)), Some(false));
        // Use coded, macroblock-padded dimensions, not the cropped display size.
        assert_eq!(bounded_sps(&sps(1920, 1088, 15)), Some(true));
        assert_eq!(bounded_sps(&sps(1920, 1088, 16)), Some(false));
        let error = ParameterSetError::ReferenceBudget {
            references: 5,
            width: 3840,
            height: 2160,
        };
        assert_eq!(inspect_sps(&sps(3840, 2160, 5)), Some(Err(error)));
    }
}

/// Complete-frame backlog. Lost dependencies discard queued work and require a
/// fresh SPS/PPS + IDR, rather than decoding a growing FIFO of stale packets.
pub mod receive;
