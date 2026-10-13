use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// One 20 ms interval. FEC borrows the successor; its normal decode is still due.
#[derive(Debug, PartialEq)]
pub enum Playout {
    Packet(bytes::Bytes),
    Fec(bytes::Bytes),
    Plc,
    Wait,
}
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
        self.push_at(sequence, payload, Instant::now());
    }
    pub fn push_at(&mut self, sequence: u16, payload: bytes::Bytes, now: Instant) {
        if payload.len() > 1500 {
            return;
        }
        let expected = *self.expected.get_or_insert_with(|| {
            self.first = now;
            self.missing = 0;
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
            self.missing = 0;
        }
        if self.packets.len() < 8 {
            self.packets.entry(sequence).or_insert(payload);
        }
    }
    pub fn pop(&mut self, deadline_reached: bool) -> Playout {
        self.pop_at(deadline_reached, Instant::now())
    }
    pub fn pop_at(&mut self, deadline_reached: bool, now: Instant) -> Playout {
        let Some(expected) = self.expected else {
            return Playout::Wait;
        };
        if !self.playing {
            if self.packets.len() < 3 && now.duration_since(self.first) < Duration::from_millis(60)
            {
                return Playout::Wait;
            }
            self.playing = true;
        }
        if !deadline_reached && !self.packets.contains_key(&expected) {
            return Playout::Wait;
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
            return Playout::Wait;
        }
        match packet {
            Some(packet) => Playout::Packet(packet),
            None => self
                .packets
                .get(&expected.wrapping_add(1))
                .cloned()
                .map_or(Playout::Plc, Playout::Fec),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn packet(seq: u16) -> bytes::Bytes {
        bytes::Bytes::from(seq.to_string())
    }
    fn push(j: &mut Jitter, sequences: &[u16], now: Instant) {
        for &seq in sequences {
            j.push_at(seq, packet(seq), now);
        }
    }
    #[test]
    fn reorders_deduplicates_and_wraps() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, &[65534, 0, 65535, 65535], now);
        for seq in [65534, 65535, 0] {
            assert_eq!(j.pop_at(true, now), Playout::Packet(packet(seq)));
        }
        assert_eq!(j.pop_at(true, now), Playout::Plc);
        push(&mut j, &[65535], now);
        assert_eq!(j.pop_at(true, now), Playout::Plc);
    }
    #[test]
    fn waits_until_deadline_then_borrows_successor_exactly_once_across_wrap() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, &[65534, 0, 1, 0], now);
        assert_eq!(j.pop_at(false, now), Playout::Packet(packet(65534)));
        for _ in 0..5 {
            assert_eq!(j.pop_at(false, now), Playout::Wait);
        }
        assert_eq!(j.pop_at(true, now), Playout::Fec(packet(0)));
        push(&mut j, &[65535, 0], now); // recovered original is now late
        assert_eq!(j.pop_at(false, now), Playout::Packet(packet(0)));
        assert_eq!(j.pop_at(false, now), Playout::Packet(packet(1)));
        assert_eq!(j.pop_at(false, now), Playout::Wait);
    }
    #[test]
    fn delayed_original_wins_before_deadline() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, &[0, 2, 3], now);
        assert_eq!(j.pop_at(false, now), Playout::Packet(packet(0)));
        assert_eq!(j.pop_at(false, now), Playout::Wait);
        push(&mut j, &[1], now);
        assert_eq!(j.pop_at(false, now), Playout::Packet(packet(1)));
    }
    #[test]
    fn burst_loss_and_late_successor_never_recover_an_interval_twice() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, &[0, 3, 4], now);
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(0)));
        assert_eq!(j.pop_at(true, now), Playout::Plc); // no immediate successor
        assert_eq!(j.pop_at(true, now), Playout::Fec(packet(3)));
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(3)));
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(4)));
        assert_eq!(j.pop_at(true, now), Playout::Plc); // 5 is committed as PLC
        push(&mut j, &[6], now);
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(6)));
    }
    #[test]
    fn startup_expiry_loss_limit_and_source_reset_are_bounded() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, &[0], now);
        assert_eq!(
            j.pop_at(true, now + Duration::from_millis(59)),
            Playout::Wait
        );
        assert_eq!(
            j.pop_at(true, now + Duration::from_millis(60)),
            Playout::Packet(packet(0))
        );
        for _ in 0..10 {
            assert_eq!(j.pop_at(true, now), Playout::Plc);
        }
        assert_eq!(j.pop_at(true, now), Playout::Wait);
        push(&mut j, &[100, 102, 103], now);
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(100)));
        assert_eq!(j.pop_at(true, now), Playout::Fec(packet(102)));
        j = Jitter::default(); // slot replacement discards the borrowed successor
        push(&mut j, &[500, 501, 502], now);
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(500)));
        push(&mut j, &[600, 601, 602], now); // excessive jump restarts buffering
        assert_eq!(j.pop_at(true, now), Playout::Packet(packet(600)));
        assert!(j.packets.len() <= 8);
    }
}
