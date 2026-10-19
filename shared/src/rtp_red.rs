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
pub struct Packet<'a> {
    pub primary: &'a [u8],
    pub history: Vec<Block<'a>>,
}

/// Padded Opus may carry DRED. Never multiply that history. Large VBR frames
/// also travel primary-only; limits apply regardless of other encoder settings.
pub fn repeatable(payload: &[u8]) -> bool {
    (3..=MAX_BLOCK).contains(&payload.len()) && !(payload[0] & 3 == 3 && payload[1] & 0x40 != 0)
}

pub fn parse(payload: &[u8]) -> Option<Packet<'_>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut index = 0;
    let mut headers = Vec::with_capacity(DEPTH);
    loop {
        let first = *payload.get(index)?;
        if first & 0x7f != OPUS_PAYLOAD_TYPE {
            return None;
        }
        index += 1;
        if first & 0x80 == 0 {
            break;
        }
        if headers.len() == DEPTH {
            return None;
        }
        let h = payload.get(index..index + 3)?;
        let offset = (u16::from(h[0]) << 6) | u16::from(h[1] >> 2);
        let length = (usize::from(h[1] & 3) << 8) | usize::from(h[2]);
        if ![FRAME as u16, (2 * FRAME) as u16].contains(&offset)
            || length == 0
            || length > MAX_BLOCK
            || headers
                .last()
                .is_some_and(|(previous, _)| *previous <= offset)
        {
            return None;
        }
        headers.push((offset, length));
        index += 3;
    }
    let mut history = Vec::with_capacity(DEPTH);
    for (offset, length) in headers {
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

pub fn encode(primary: &[u8], history: &[Block<'_>]) -> Option<Vec<u8>> {
    let size = 1 + primary.len() + history.iter().map(|b| 4 + b.payload.len()).sum::<usize>();
    if primary.is_empty() || history.len() > DEPTH || size > MAX_PAYLOAD {
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
    parse(&result)?;
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
