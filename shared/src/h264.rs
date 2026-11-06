//! Portable H.264 framing and decoder allocation bounds shared by real and test viewers.
use crate::screen::{MAX_FRAME_BYTES, MAX_HEIGHT, MAX_WIDTH};

/// Offset of the first `00 00 01` start code. Encoded frames are scanned several
/// times per frame on the send and receive paths; inspecting every third byte
/// skips most of the slice data (a byte above 1 rules out three positions).
fn start_code(data: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 2 < data.len() {
        match data[i + 2] {
            0 => i += 1,
            1 if data[i] == 0 && data[i + 1] == 0 => return Some(i),
            _ => i += 3,
        }
    }
    None
}

/// Annex-B NAL units, without three- or four-byte start codes.
pub fn nal_units(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let start = start_code(data)? + 3;
        data = &data[start..];
        let end = start_code(data).unwrap_or(data.len());
        let mut nal = &data[..end];
        data = &data[end..];
        while nal.last() == Some(&0) {
            nal = &nal[..nal.len() - 1];
        }
        Some(nal)
    })
}

/// An IDR access unit with its SPS and PPS. Stops at the first non-IDR slice
/// (an access unit cannot mix IDR and non-IDR slices) or once all three are
/// seen, so delta frames are classified without scanning their slice data.
pub fn recovery_frame(mut data: &[u8]) -> bool {
    let mut kinds = [false; 32];
    // Only each NAL header byte is needed: resume the next start-code search at
    // the header instead of first finding where the slice data ends.
    while let Some(start) = start_code(data) {
        data = &data[start + 3..];
        let Some(byte) = data.first() else {
            break;
        };
        match byte & 31 {
            0 => {}
            1 => return false,
            kind => kinds[kind as usize] = true,
        }
        if kinds[5] && kinds[7] && kinds[8] {
            return true;
        }
    }
    false
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
    pub fn is_damaged(&self) -> bool {
        self.damaged
    }
    pub fn push(
        &mut self,
        timestamp: u32,
        sequence: u16,
        marker: bool,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        if self.timestamp != Some(timestamp) {
            self.timestamp = Some(timestamp);
            self.damaged = false;
            self.fragment = false;
            self.data.clear();
        }
        if self.next.is_some_and(|n| n != sequence) {
            self.damaged = true;
        }
        self.next = Some(sequence.wrapping_add(1));
        let p = payload;
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
        if marker && !self.fragment {
            // The frame leaves with its buffer; start the next one at this
            // size so it does not regrow through repeated reallocations.
            let capacity = self.data.len();
            return Some(std::mem::replace(
                &mut self.data,
                Vec::with_capacity(capacity),
            ));
        }
        None
    }
    fn nal(&mut self, data: &[u8]) {
        self.data.extend_from_slice(&[0, 0, 0, 1]);
        self.data.extend_from_slice(data);
    }
}

// Preserve the previous worst-case reference-picture budget (four 4K frames),
// while allowing smaller camera frames to use H.264's limit of 16 references.
const MAX_REFERENCE_PIXELS: u64 = MAX_WIDTH as u64 * (MAX_HEIGHT.div_ceil(16) * 16) as u64 * 4;

/// Restrict remote SPS dimensions/reference counts before the video decoder can
/// allocate its picture buffers. Only progressive baseline is negotiated here.
pub fn bounded_parameter_sets(data: &[u8]) -> bool {
    validate_parameter_sets(data).is_ok()
}

/// Bitstream profile class. Constrained baseline remains the compatibility floor;
/// 8-bit 4:2:0 High is accepted only where the receiving decoder negotiated it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Baseline,
    High,
}

/// Same allocation bounds as [`bounded_parameter_sets`], optionally admitting High.
pub fn bounded_parameter_sets_with(data: &[u8], allow_high: bool) -> bool {
    parameter_set_profile(data, allow_high).is_ok()
}

/// Calls `unit` for each NAL unit up to and including the first slice (types
/// 1–5), which is passed as its header byte only. Encoders emit SPS/PPS/SEI
/// before the first slice, so this reads an encoded frame's parameter sets and
/// picture type without scanning its slice data. For this process's own encoder
/// output; received frames are validated with the full [`parameter_set_profile`].
pub fn front_units(mut data: &[u8], mut unit: impl FnMut(&[u8])) {
    while let Some(start) = start_code(data) {
        data = &data[start + 3..];
        let Some(&header) = data.first() else {
            return;
        };
        if matches!(header & 31, 1..=5) {
            unit(&data[..1]);
            return;
        }
        let end = start_code(data).unwrap_or(data.len());
        let mut nal = &data[..end];
        while nal.last() == Some(&0) {
            nal = &nal[..nal.len() - 1];
        }
        if !nal.is_empty() {
            unit(nal);
        }
        data = &data[end..];
    }
}
/// Validates one SPS NAL unit (header byte included) like [`parameter_set_profile`].
pub fn sps_profile(nal: &[u8], allow_high: bool) -> Result<Profile, ParameterSetError> {
    inspect_sps(nal, allow_high).ok_or(ParameterSetError::Malformed)?
}
/// [`parameter_set_profile`] over [`front_units`]: own encoder output only.
pub fn front_parameter_set_profile(
    data: &[u8],
    allow_high: bool,
) -> Result<Option<Profile>, ParameterSetError> {
    let mut result = Ok(None);
    front_units(data, |nal| {
        if result.is_ok() && nal[0] & 31 == 7 {
            result = inspect_sps(nal, allow_high)
                .ok_or(ParameterSetError::Malformed)
                .and_then(|r| r.map(Some));
        }
    });
    result
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
    /// High was negotiated, but this SPS is neither baseline nor 8-bit 4:2:0 High.
    HighProfile {
        id: u8,
        constraints: u8,
        level: u8,
    },
    /// High chroma format, bit depth or scaling-list field outside 8-bit 4:2:0.
    HighFormat,
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
            Self::HighProfile {
                id,
                constraints,
                level,
            } => write!(
                f,
                "unsupported H.264 profile-level-id={id:02x}{constraints:02x}{level:02x}; expected baseline or high"
            ),
            Self::HighFormat => write!(
                f,
                "unsupported H.264 high chroma format, bit depth or scaling list; expected 8-bit 4:2:0"
            ),
        }
    }
}

pub fn validate_parameter_sets(data: &[u8]) -> Result<(), ParameterSetError> {
    parameter_set_profile(data, false).map(|_| ())
}
/// Validates every SPS and reports the last one's profile; `None` without an SPS.
pub fn parameter_set_profile(
    data: &[u8],
    allow_high: bool,
) -> Result<Option<Profile>, ParameterSetError> {
    let mut profile = None;
    for nal in nal_units(data) {
        if nal.first().is_none_or(|b| b & 31 != 7) {
            continue;
        }
        profile = Some(inspect_sps(nal, allow_high).ok_or(ParameterSetError::Malformed)??);
    }
    Ok(profile)
}
#[cfg(test)]
fn bounded_sps(nal: &[u8]) -> Option<bool> {
    inspect_sps(nal, false).map(|result| result.is_ok())
}
fn inspect_sps(nal: &[u8], allow_high: bool) -> Option<Result<Profile, ParameterSetError>> {
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
    // High (profile_idc 100) adds CABAC and 8x8 transforms. Constrained High
    // (constraint_set4/5) is preferred, but the publisher also disables
    // B-frames, so plain High from a driver that ignores the constraint is
    // accepted; reordering never changes the decoder allocation bounds below.
    let high = allow_high && id == 100;
    if !baseline && !high {
        return Some(Err(if allow_high {
            ParameterSetError::HighProfile {
                id,
                constraints,
                level,
            }
        } else {
            ParameterSetError::Profile {
                id,
                constraints,
                level,
            }
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
    if high {
        let chroma = bits.ue()?;
        let luma_depth = bits.ue()?;
        let chroma_depth = bits.ue()?;
        bits.bit()?; // qpprime_y_zero_transform_bypass_flag
        if chroma != 1 || luma_depth != 0 || chroma_depth != 0 {
            return Some(Err(ParameterSetError::HighFormat));
        }
        if bits.bit()? {
            // Eight 4:2:0 scaling lists; consumed only to reach the bounded fields.
            for list in 0..8 {
                if bits.bit()? {
                    let size = if list < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8_i32, 8_i32);
                    for _ in 0..size {
                        if next != 0 {
                            let delta = bits.se()?;
                            if !(-128..=127).contains(&delta) {
                                return Some(Err(ParameterSetError::HighFormat));
                            }
                            next = (last + delta + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
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
        Ok(if high {
            Profile::High
        } else {
            Profile::Baseline
        })
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
    fn se(&mut self) -> Option<i32> {
        let value = self.ue()?;
        let magnitude = i32::try_from(value.div_ceil(2)).ok()?;
        Some(if value % 2 == 1 {
            magnitude
        } else {
            -magnitude
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The skipping scan must split exactly like the original byte-window scan.
    #[test]
    fn skipping_start_code_scan_matches_window_scan() {
        fn reference(mut data: &[u8]) -> Vec<&[u8]> {
            let mut out = Vec::new();
            while let Some(start) = data.windows(3).position(|p| p == [0, 0, 1]) {
                data = &data[start + 3..];
                let end = data
                    .windows(3)
                    .position(|p| p == [0, 0, 1])
                    .unwrap_or(data.len());
                let mut nal = &data[..end];
                data = &data[end..];
                while nal.last() == Some(&0) {
                    nal = &nal[..nal.len() - 1];
                }
                out.push(nal);
            }
            out
        }
        // Deterministic bytes biased toward 0 and 1 so start codes, emulation
        // patterns and runs of zeros are common.
        let mut state = 0x2545_f491_u32;
        for length in 0..600 {
            let data: Vec<u8> = (0..length)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    match state % 8 {
                        0..=3 => 0,
                        4 | 5 => 1,
                        _ => (state >> 8) as u8,
                    }
                })
                .collect();
            let nals = reference(&data);
            assert_eq!(nal_units(&data).collect::<Vec<_>>(), nals);
            // The former full-scan classification, except that a non-IDR
            // slice now ends the scan as not recoverable.
            let kinds: Vec<u8> = nals
                .iter()
                .filter_map(|n| n.first().map(|b| b & 31))
                .collect();
            let full = [5, 7, 8].iter().all(|k| kinds.contains(k));
            let first_delta = kinds.iter().position(|k| *k == 1);
            let before_delta = |k: u8| kinds[..first_delta.unwrap_or(kinds.len())].contains(&k);
            let expected = full && [5, 7, 8].into_iter().all(before_delta);
            assert_eq!(recovery_frame(&data), expected);
        }
    }
    #[test]
    fn front_units_match_nal_units_up_to_the_first_slice() {
        let mut frame = vec![
            0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x67, 66, 0, 0, 0, 1, 0x68, 7,
        ];
        frame.extend([0, 0, 1, 0x06, 5, 0, 0, 1, 0x65]);
        frame.extend(std::iter::repeat_n(0x55, 10_000));
        frame.extend([0, 0, 1, 0x67, 1]); // after the slice: not a front unit
        let mut front = Vec::new();
        front_units(&frame, |nal| front.push(nal.to_vec()));
        let expected: Vec<Vec<u8>> = nal_units(&frame)
            .take(4)
            .map(<[u8]>::to_vec)
            .chain([vec![0x65]])
            .collect();
        assert_eq!(front, expected);
        assert_eq!(
            front_parameter_set_profile(&frame[..10], false),
            Err(ParameterSetError::Malformed),
            "a truncated SPS is rejected"
        );
        assert_eq!(
            front_parameter_set_profile(&[0, 0, 1, 0x41, 1], false),
            Ok(None)
        );
    }
    #[test]
    fn recovery_frames_need_idr_sps_and_pps_and_deltas_stop_early() {
        let idr = [0, 0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1];
        assert!(recovery_frame(&idr));
        assert!(!recovery_frame(&idr[..10]), "no IDR slice");
        assert!(
            !recovery_frame(&[0, 0, 1, 0x65, 1]),
            "IDR without parameter sets"
        );
        // Parameter sets after the first IDR slice still count.
        assert!(recovery_frame(&[
            0, 0, 1, 0x65, 1, 0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1
        ]));
        let mut delta = vec![0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x41];
        delta.extend(std::iter::repeat_n(0x55, 100_000));
        assert!(!recovery_frame(&delta));
    }
    #[test]
    fn assembled_frames_leave_with_their_buffer_and_the_next_starts_presized() {
        let mut assembler = Assembler::default();
        let payload = [0x41_u8; 900];
        assert!(assembler.push(1, 1, false, &payload).is_none());
        let frame = assembler.push(1, 2, true, &payload).unwrap();
        assert_eq!(frame.len(), 2 * (4 + payload.len()));
        assert!(assembler.data.is_empty());
        assert!(assembler.data.capacity() >= frame.len());
    }
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
    fn high_profile_is_opt_in_and_keeps_allocation_bounds() {
        fn ue(bits: &mut String, value: u32) {
            let code = format!("{:b}", value + 1);
            bits.push_str(&"0".repeat(code.len() - 1));
            bits.push_str(&code);
        }
        fn se(bits: &mut String, value: i32) {
            ue(
                bits,
                if value > 0 {
                    value as u32 * 2 - 1
                } else {
                    (-value) as u32 * 2
                },
            );
        }
        // chroma, luma depth, chroma depth, scaling lists (None = absent).
        fn sps(format: (u32, u32, u32), lists: Option<&[i32]>, refs: u32, width: u32) -> Vec<u8> {
            let mut bits = String::new();
            ue(&mut bits, 0); // SPS id
            ue(&mut bits, format.0);
            ue(&mut bits, format.1);
            ue(&mut bits, format.2);
            bits.push('0'); // qpprime bypass
            match lists {
                None => bits.push('0'),
                Some(deltas) => {
                    bits.push('1');
                    for list in 0..8 {
                        // Present lists stop early at a zero next-scale.
                        bits.push(if list % 2 == 0 { '1' } else { '0' });
                        if list % 2 == 0 {
                            let size = if list < 6 { 16 } else { 64 };
                            for &delta in deltas.iter().take(size) {
                                se(&mut bits, delta);
                            }
                        }
                    }
                }
            }
            ue(&mut bits, 0); // log2_max_frame_num_minus4
            ue(&mut bits, 2); // picture order type
            ue(&mut bits, refs);
            bits.push('0');
            ue(&mut bits, width / 16 - 1);
            ue(&mut bits, 67); // 1088 coded rows
            bits.push_str("11001");
            while !bits.len().is_multiple_of(8) {
                bits.push('0');
            }
            let mut nal = vec![0x67, 100, 0x0c, 52];
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
        let frame = |nal: Vec<u8>| [vec![0, 0, 0, 1], nal].concat();
        let plain = frame(sps((1, 0, 0), None, 4, 1920));
        // Older receivers and non-negotiated viewers keep rejecting High.
        assert!(!bounded_parameter_sets(&plain));
        assert_eq!(
            validate_parameter_sets(&plain),
            Err(ParameterSetError::Profile {
                id: 100,
                constraints: 0x0c,
                level: 52
            })
        );
        assert_eq!(parameter_set_profile(&plain, true), Ok(Some(Profile::High)));
        // A zero delta at the first entry ends the list (8 -> 0).
        let scaled = frame(sps((1, 0, 0), Some(&[-8]), 4, 1920));
        assert_eq!(
            parameter_set_profile(&scaled, true),
            Ok(Some(Profile::High))
        );
        let full = frame(sps((1, 0, 0), Some(&[1; 64]), 4, 1920));
        assert_eq!(parameter_set_profile(&full, true), Ok(Some(Profile::High)));
        assert_eq!(
            parameter_set_profile(&frame(sps((1, 0, 0), Some(&[200]), 4, 1920)), true),
            Err(ParameterSetError::HighFormat)
        );
        for format in [(0, 0, 0), (2, 0, 0), (3, 0, 0), (1, 2, 0), (1, 0, 2)] {
            assert_eq!(
                parameter_set_profile(&frame(sps(format, None, 4, 1920)), true),
                Err(ParameterSetError::HighFormat)
            );
        }
        // The same reference/dimension budget applies after the High fields.
        assert_eq!(
            parameter_set_profile(&frame(sps((1, 0, 0), None, 17, 1920)), true),
            Err(ParameterSetError::References(17))
        );
        assert_eq!(
            parameter_set_profile(&frame(sps((1, 0, 0), None, 1, 4096)), true),
            Err(ParameterSetError::Dimensions {
                width: 4096,
                height: 1088
            })
        );
        let nal = sps((1, 0, 0), Some(&[1; 64]), 4, 1920);
        // The final byte holds only the progressive/stop bits after padding.
        for end in 1..nal.len() - 2 {
            assert!(parameter_set_profile(&frame(nal[..end].to_vec()), true).is_err());
        }
        // Baseline stays accepted (and reported) when High is allowed.
        let mut baseline = vec![0, 0, 0, 1, 0x67, 66, 0xe0, 31];
        baseline.extend([0xf4, 0x05, 0x01, 0x6c, 0x80]);
        assert_eq!(
            parameter_set_profile(&baseline, true),
            parameter_set_profile(&baseline, false)
        );
        assert_eq!(parameter_set_profile(&[0, 0, 1, 0x65, 1], true), Ok(None));
        assert_eq!(
            parameter_set_profile(&frame(vec![0x67, 77, 0, 31, 0xf4]), true)
                .unwrap_err()
                .to_string(),
            "unsupported H.264 profile-level-id=4d001f; expected baseline or high"
        );
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
        assert_eq!(inspect_sps(&sps(3840, 2160, 5), false), Some(Err(error)));
    }
}
