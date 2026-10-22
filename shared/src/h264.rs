//! Portable H.264 framing and decoder allocation bounds shared by real and test viewers.
use crate::screen::{MAX_FRAME_BYTES, MAX_HEIGHT, MAX_WIDTH};

/// Annex-B NAL units, without three- or four-byte start codes.
pub fn nal_units(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let start = data.windows(3).position(|p| p == [0, 0, 1])? + 3;
        data = &data[start..];
        let end = data
            .windows(3)
            .position(|p| p == [0, 0, 1])
            .unwrap_or(data.len());
        let mut nal = &data[..end];
        data = &data[end..];
        while nal.last() == Some(&0) {
            nal = &nal[..nal.len() - 1];
        }
        Some(nal)
    })
}

pub fn recovery_frame(data: &[u8]) -> bool {
    let mut kinds = [false; 32];
    for nal in nal_units(data) {
        if let Some(byte) = nal.first() {
            kinds[(byte & 31) as usize] = true;
        }
    }
    kinds[5] && kinds[7] && kinds[8]
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
            return Some(std::mem::take(&mut self.data));
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
    for nal in nal_units(data) {
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
    use super::*;
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
