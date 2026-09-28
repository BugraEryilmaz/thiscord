use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
/// 20 ms Opus packets, at most eight buffered packets per source. Sequence wrap
/// is intentional. Late/duplicate packets never rewind decoder state.
pub struct Jitter {
    packets: HashMap<u16, bytes::Bytes>,
    expected: Option<u16>,
    first: Instant,
    playing: bool,
    missing: u8,
}
impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: HashMap::new(),
            expected: None,
            first: Instant::now(),
            playing: false,
            missing: 0,
        }
    }
}
impl Jitter {
    pub fn push(&mut self, sequence: u16, payload: bytes::Bytes) {
        if payload.len() > 1500 {
            return;
        }
        let expected = *self.expected.get_or_insert_with(|| {
            self.first = Instant::now();
            sequence
        });
        let distance = sequence.wrapping_sub(expected) as i16;
        if distance < 0 {
            return;
        }
        if distance > 8 {
            self.packets.clear();
            self.expected = Some(sequence);
            self.playing = false;
            self.first = Instant::now();
        }
        if self.packets.len() < 8 {
            self.packets.entry(sequence).or_insert(payload);
        }
    }
    pub fn pop(&mut self) -> Option<Option<bytes::Bytes>> {
        let expected = self.expected?;
        if !self.playing {
            if self.packets.len() < 3 && self.first.elapsed() < Duration::from_millis(60) {
                return None;
            }
            self.playing = true;
        }
        let packet = self.packets.remove(&expected);
        if packet.is_some() {
            self.missing = 0;
        } else {
            self.missing += 1;
        }
        self.expected = Some(expected.wrapping_add(1));
        if self.missing > 10 {
            self.expected = None;
            self.playing = false;
            self.packets.clear();
            return None;
        }
        Some(packet)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reorders_deduplicates_and_wraps() {
        let mut j = Jitter::default();
        for i in [65534, 0, 65535, 65535] {
            j.push(i, bytes::Bytes::from(i.to_string()));
        }
        for i in [65534, 65535, 0] {
            assert_eq!(j.pop().unwrap().unwrap(), i.to_string());
        }
        assert!(j.pop().unwrap().is_none());
        j.push(65535, bytes::Bytes::from_static(b"late"));
        assert!(j.pop().unwrap().is_none());
    }
}
