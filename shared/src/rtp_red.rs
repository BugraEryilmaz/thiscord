//! Bounded RFC 2198 microphone profile. No RTP sequence or marker is invented
//! for redundant blocks; their only media identity is the timestamp offset.
pub const PAYLOAD_TYPE: u8 = 112;
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
pub const MIME: &str = "audio/red";
pub const FMTP: &str = "111/111/111";
pub const FRAME: u32 = 960;
pub const DEPTH: usize = 2;
pub const MAX_PAYLOAD: usize = 1100;
pub const MAX_BLOCK: usize = 160;

#[derive(Clone, Copy)]
pub struct Block<'a> {
    pub offset: u16,
    pub payload: &'a [u8],
}
const EMPTY: Block<'static> = Block {
    offset: 0,
    payload: &[],
};

/// At most [`DEPTH`] redundant blocks, oldest first, stored inline so parsing
/// and re-encoding a packet never touches the heap. Dereferences to a slice.
#[derive(Clone, Copy)]
pub struct History<'a> {
    blocks: [Block<'a>; DEPTH],
    len: usize,
}
impl<'a> History<'a> {
    pub const fn new() -> Self {
        Self {
            blocks: [EMPTY; DEPTH],
            len: 0,
        }
    }
    /// Returns false, leaving the history unchanged, once it holds DEPTH blocks.
    pub fn push(&mut self, block: Block<'a>) -> bool {
        let Some(slot) = self.blocks.get_mut(self.len) else {
            return false;
        };
        *slot = block;
        self.len += 1;
        true
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&Block<'a>) -> bool) {
        let mut kept = 0;
        for index in 0..self.len {
            if keep(&self.blocks[index]) {
                self.blocks[kept] = self.blocks[index];
                kept += 1;
            }
        }
        self.len = kept;
    }
}
impl Default for History<'_> {
    fn default() -> Self {
        Self::new()
    }
}
impl<'a> std::ops::Deref for History<'a> {
    type Target = [Block<'a>];
    fn deref(&self) -> &Self::Target {
        &self.blocks[..self.len]
    }
}
impl<'a> IntoIterator for History<'a> {
    type Item = Block<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<Block<'a>, DEPTH>>;
    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_iter().take(self.len)
    }
}

pub struct Packet<'a> {
    pub primary: &'a [u8],
    pub history: History<'a>,
}

/// Padded Opus may carry DRED. Never multiply that history. Large VBR frames
/// also travel primary-only; limits apply regardless of other encoder settings.
pub fn repeatable(payload: &[u8]) -> bool {
    (3..=MAX_BLOCK).contains(&payload.len()) && !(payload[0] & 3 == 3 && payload[1] & 0x40 != 0)
}

fn valid_offset(offset: u16) -> bool {
    offset == FRAME as u16 || offset == (2 * FRAME) as u16
}

/// Allocation-free; runs for every received RED packet on the audio worker and
/// every forwarded RED packet in the SFU.
pub fn parse(payload: &[u8]) -> Option<Packet<'_>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut index = 0;
    let mut headers = [(0u16, 0usize); DEPTH];
    let mut count = 0;
    loop {
        let first = *payload.get(index)?;
        if first & 0x7f != OPUS_PAYLOAD_TYPE {
            return None;
        }
        index += 1;
        if first & 0x80 == 0 {
            break;
        }
        if count == DEPTH {
            return None;
        }
        let h = payload.get(index..index + 3)?;
        let offset = (u16::from(h[0]) << 6) | u16::from(h[1] >> 2);
        let length = (usize::from(h[1] & 3) << 8) | usize::from(h[2]);
        if !valid_offset(offset)
            || length == 0
            || length > MAX_BLOCK
            || (count > 0 && headers[count - 1].0 <= offset)
        {
            return None;
        }
        headers[count] = (offset, length);
        count += 1;
        index += 3;
    }
    let mut history = History::new();
    for &(offset, length) in &headers[..count] {
        let block = payload.get(index..index + length)?;
        if !repeatable(block) {
            return None;
        }
        history.push(Block {
            offset,
            payload: block,
        });
        index += length;
    }
    let primary = payload.get(index..)?;
    if primary.is_empty() || (!history.is_empty() && !repeatable(primary)) {
        return None;
    }
    Some(Packet { primary, history })
}

/// Applies `parse`'s acceptance rules to the inputs, so every result parses
/// without re-reading the encoded bytes.
pub fn encode(primary: &[u8], history: &[Block<'_>]) -> Option<Vec<u8>> {
    let size = 1 + primary.len() + history.iter().map(|b| 4 + b.payload.len()).sum::<usize>();
    if primary.is_empty()
        || history.len() > DEPTH
        || size > MAX_PAYLOAD
        || (!history.is_empty() && !repeatable(primary))
        || history
            .iter()
            .any(|b| !valid_offset(b.offset) || !repeatable(b.payload))
        || history.windows(2).any(|w| w[0].offset <= w[1].offset)
    {
        return None;
    }
    let mut result = Vec::with_capacity(size);
    for b in history {
        let n = b.payload.len();
        result.extend_from_slice(&[
            OPUS_PAYLOAD_TYPE | 0x80,
            (b.offset >> 6) as u8,
            ((b.offset & 0x3f) << 2) as u8 | (n >> 8) as u8,
            n as u8,
        ]);
    }
    result.push(OPUS_PAYLOAD_TYPE);
    for b in history {
        result.extend_from_slice(b.payload);
    }
    result.extend_from_slice(primary);
    debug_assert!(parse(&result).is_some());
    Some(result)
}

/// Fixed mapping is accepted only when every audio m-line explicitly negotiates
/// both formats and the RFC's slash-separated fmtp. Fail closed on partial SDP.
pub fn negotiated(sdp: &str) -> bool {
    let mut audio = 0;
    for section in sdp.split("m=").skip(1) {
        let mut lines = section.lines();
        let first = lines.next().unwrap_or_default();
        if !first.starts_with("audio ") {
            continue;
        }
        audio += 1;
        let formats: Vec<_> = first.split_whitespace().skip(3).collect();
        let lines: Vec<_> = lines.map(str::trim).collect();
        if !formats.contains(&"111")
            || !formats.contains(&"112")
            || !lines
                .iter()
                .any(|l| l.eq_ignore_ascii_case("a=rtpmap:112 red/48000/2"))
            || !lines
                .iter()
                .any(|l| l.eq_ignore_ascii_case("a=rtpmap:111 opus/48000/2"))
            || !lines.contains(&"a=fmtp:112 111/111/111")
        {
            return false;
        }
    }
    audio > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc_layout_and_bounds() {
        let p = [0x78, 1, 2];
        let encoded = encode(
            &p,
            &[
                Block {
                    offset: 1920,
                    payload: &p,
                },
                Block {
                    offset: 960,
                    payload: &p,
                },
            ],
        )
        .unwrap();
        assert_eq!(&encoded[..9], &[239, 30, 0, 3, 239, 15, 0, 3, 111]);
        let parsed = parse(&encoded).unwrap();
        assert_eq!(parsed.primary, p);
        assert_eq!(parsed.history[0].offset, 1920);
        for n in 0..9 {
            assert!(parse(&encoded[..n]).is_none());
        }
        assert!(parse(&[111]).is_none());
        assert!(parse(&[112, 1]).is_none());
        assert!(parse(&vec![111; MAX_PAYLOAD + 1]).is_none());
        assert!(
            encode(
                &p,
                &[Block {
                    offset: 0,
                    payload: &p
                }]
            )
            .is_none()
        );
        assert!(!repeatable(&[3, 0x40, 1]));
        assert!(!repeatable(&[0x78, 0]));
        assert!(
            encode(
                &p,
                &[Block {
                    offset: 960,
                    payload: &[1; 161]
                }]
            )
            .is_none()
        );
    }
    #[test]
    fn untrusted_primary_boundaries_cannot_advertise_prior_recovery() {
        for primary in [vec![3, 0x40, 1], vec![0x78], vec![0x78, 0], vec![0x78; 161]] {
            let mut packet = vec![239, 15, 0, 3, 111, 0x78, 1, 2];
            packet.extend_from_slice(&primary);
            assert!(parse(&packet).is_none());
            assert!(
                parse(&encode(&primary, &[]).unwrap())
                    .unwrap()
                    .history
                    .is_empty()
            );
        }
    }
    /// `encode` validates its inputs instead of re-parsing its output; both
    /// must accept exactly the same packets.
    #[test]
    fn encode_accepts_exactly_what_parse_accepts() {
        fn naive(primary: &[u8], history: &[Block<'_>]) -> Vec<u8> {
            let mut out = Vec::new();
            for b in history {
                let n = b.payload.len();
                out.extend_from_slice(&[
                    OPUS_PAYLOAD_TYPE | 0x80,
                    (b.offset >> 6) as u8,
                    ((b.offset & 0x3f) << 2) as u8 | (n >> 8) as u8,
                    n as u8,
                ]);
            }
            out.push(OPUS_PAYLOAD_TYPE);
            for b in history {
                out.extend_from_slice(b.payload);
            }
            out.extend_from_slice(primary);
            out
        }
        let payloads: [Vec<u8>; 8] = [
            vec![],
            vec![0x78],
            vec![0x78, 1],
            vec![0x78, 1, 2],
            vec![3, 0x40, 1],
            vec![0x78; MAX_BLOCK],
            vec![0x78; MAX_BLOCK + 1],
            vec![0x78; 700],
        ];
        let offsets = [0u16, 960, 1920, 2880];
        let mut checked = 0;
        for primary in &payloads {
            for depth in 0..=DEPTH + 1 {
                let combos = (offsets.len() * payloads.len()).pow(depth as u32);
                for mut code in 0..combos {
                    let history: Vec<_> = (0..depth)
                        .map(|_| {
                            let choice = code % (offsets.len() * payloads.len());
                            code /= offsets.len() * payloads.len();
                            Block {
                                offset: offsets[choice % offsets.len()],
                                payload: &payloads[choice / offsets.len()],
                            }
                        })
                        .collect();
                    let reference = naive(primary, &history);
                    let parses = depth <= DEPTH
                        && history.iter().all(|b| b.payload.len() < 1024)
                        && parse(&reference).is_some();
                    match encode(primary, &history) {
                        Some(encoded) => {
                            assert!(parses);
                            assert_eq!(encoded, reference);
                        }
                        None => assert!(!parses),
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000);
    }
    #[test]
    fn inline_history_retains_order_and_bounds() {
        let a = [0x78, 1, 2];
        let mut history = History::new();
        assert!(history.push(Block {
            offset: 1920,
            payload: &a
        }));
        assert!(history.push(Block {
            offset: 960,
            payload: &a
        }));
        assert!(!history.push(Block {
            offset: 960,
            payload: &a
        }));
        assert_eq!(history.len(), DEPTH);
        history.retain(|b| b.offset == 960);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].offset, 960);
        assert_eq!(history.into_iter().count(), 1);
    }
    #[test]
    fn fmtp_is_required_on_all_audio_sections() {
        let s = "m=audio 9 UDP/TLS/RTP/SAVPF 111 112\r\na=rtpmap:111 opus/48000/2\r\na=rtpmap:112 red/48000/2\r\na=fmtp:112 111/111/111\r\n";
        assert!(negotiated(s));
        assert!(!negotiated(&s.replace("111/111/111", "111")));
        assert!(!negotiated(&format!(
            "{s}m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"
        )));
    }
}
