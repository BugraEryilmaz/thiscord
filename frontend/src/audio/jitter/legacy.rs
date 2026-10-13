//! Frozen pre-adaptive algorithm, with its wall clock injected for trace comparison.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
/// 20 ms Opus packets, at most eight buffered packets per source. Sequence wrap
/// is intentional. Late/duplicate packets never rewind decoder state.
pub struct Legacy {
    packets: HashMap<u16, bytes::Bytes>,
    expected: Option<u16>,
    first: Instant,
    playing: bool,
    missing: u8,
}
impl Default for Legacy {
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
impl Legacy {
    pub fn push(&mut self, sequence: u16, payload: bytes::Bytes, now: Instant) {
        if payload.len() > 1500 {
            return;
        }
        let expected = *self.expected.get_or_insert_with(|| {
            self.first = now;
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
            self.first = now;
        }
        if self.packets.len() < 8 {
            self.packets.entry(sequence).or_insert(payload);
        }
    }
    pub fn pop(&mut self, conceal_missing: bool, now: Instant) -> Option<Option<bytes::Bytes>> {
        let expected = self.expected?;
        if !self.playing {
            if self.packets.len() < 3
                && now.saturating_duration_since(self.first) < Duration::from_millis(60)
            {
                return None;
            }
            self.playing = true;
        }
        if !conceal_missing && !self.packets.contains_key(&expected) {
            return None;
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
