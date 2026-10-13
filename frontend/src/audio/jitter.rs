use super::quality::{Count, Metrics};
use std::sync::Arc;
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
    pub metrics: Arc<Metrics>,
    highest: Option<u16>,
    seen: u64,
}
impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: HashMap::new(),
            expected: None,
            first: Instant::now(),
            playing: false,
            missing: 0,
            metrics: Arc::default(),
            highest: None,
            seen: 0,
        }
    }
}
impl Jitter {
    pub fn push(&mut self, sequence: u16, payload: bytes::Bytes) {
        self.push_at(sequence, payload, Instant::now());
    }
    pub fn len(&self) -> usize {
        self.packets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
    pub fn playing(&self) -> bool {
        self.playing
    }
    pub fn clear(&mut self) {
        self.metrics
            .add(Count::ResyncDiscardedPackets, self.packets.len() as u64);
        self.packets.clear();
        self.expected = None;
        self.playing = false;
        self.missing = 0;
        self.highest = None;
        self.seen = 0;
    }
    fn push_at(&mut self, sequence: u16, payload: bytes::Bytes, now: Instant) {
        self.metrics.add(Count::ReceivedPackets, 1);
        if payload.is_empty() || payload.len() > 1500 {
            self.metrics.add(Count::InvalidPackets, 1);
            return;
        }
        if let Some(highest) = self.highest {
            let delta = sequence.wrapping_sub(highest) as i16;
            if delta > 0 {
                self.seen = self.seen.checked_shl(delta as u32).unwrap_or(0) | 1;
                self.highest = Some(sequence);
            } else {
                let behind = -(i32::from(delta));
                if behind < 64 {
                    let bit = 1u64 << behind;
                    if self.seen & bit != 0 {
                        self.metrics.add(Count::DuplicatePackets, 1);
                        return;
                    }
                    self.seen |= bit;
                }
                self.metrics.add(Count::ReorderedPackets, 1);
            }
        } else {
            self.highest = Some(sequence);
            self.seen = 1;
        }
        let expected = *self.expected.get_or_insert_with(|| {
            self.first = now;
            sequence
        });
        let distance = sequence.wrapping_sub(expected) as i16;
        if distance < 0 {
            self.metrics.add(Count::LatePackets, 1);
            return;
        }
        if distance > 8 {
            self.metrics
                .add(Count::ResyncDiscardedPackets, self.packets.len() as u64);
            self.packets.clear();
            self.expected = Some(sequence);
            self.playing = false;
            self.first = now;
        }
        if self.packets.len() < 8 {
            self.packets.entry(sequence).or_insert(payload);
        } else {
            self.metrics.add(Count::JitterOverflowPackets, 1);
        }
    }
    pub fn pop(&mut self, conceal_missing: bool) -> Option<Option<bytes::Bytes>> {
        self.pop_at(conceal_missing, Instant::now())
    }
    fn pop_at(&mut self, conceal_missing: bool, now: Instant) -> Option<Option<bytes::Bytes>> {
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
            if !self.packets.is_empty() {
                self.metrics.add(Count::SequenceGapPackets, 1);
            }
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
    fn exact_arrival_policy_counts_with_wrap_and_no_tail_loss() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in [65534, 0, 65535, 65535] {
            j.push_at(seq, bytes::Bytes::from_static(b"packet"), now);
        }
        for _ in 0..3 {
            assert!(j.pop_at(true, now).unwrap().is_some());
        }
        j.push_at(65533, bytes::Bytes::from_static(b"late"), now);
        j.push_at(1, bytes::Bytes::new(), now);
        for seq in [3, 4] {
            j.push_at(seq, bytes::Bytes::from_static(b"packet"), now);
        }
        assert!(j.pop_at(true, now).unwrap().is_none());
        assert!(j.pop_at(true, now).unwrap().is_none());
        for _ in 0..2 {
            assert!(j.pop_at(true, now).unwrap().is_some());
        }
        for _ in 0..20 {
            j.pop_at(true, now);
        }
        let t = j.metrics.snapshot().total;
        assert_eq!(
            (
                t.received_packets,
                t.duplicate_packets,
                t.reordered_packets,
                t.late_packets,
                t.invalid_packets,
                t.sequence_gap_packets
            ),
            (8, 1, 2, 1, 1, 2)
        );
        assert_eq!(t.fec_attempts, 0);
    }
    #[test]
    fn buffer_policy_drops_are_not_network_loss_and_sources_are_independent() {
        let now = Instant::now();
        let mut a = Jitter::default();
        let b = Jitter::default();
        for seq in 0..10 {
            a.push_at(seq, bytes::Bytes::from_static(b"p"), now);
        }
        assert!(a.len() <= 8);
        let t = a.metrics.snapshot().total;
        assert_eq!(
            (
                t.jitter_overflow_packets,
                t.resync_discarded_packets,
                t.sequence_gap_packets
            ),
            (1, 8, 0)
        );
        assert_eq!(b.metrics.snapshot().total.received_packets, 0);
        a.clear();
        assert_eq!(a.metrics.snapshot().total.resync_discarded_packets, 9);
        assert_eq!(a.metrics.snapshot().total.received_packets, 10);
        a = Jitter::default(); // Source reassignment.
        assert_eq!(a.metrics.snapshot().total.received_packets, 0);
    }
    #[test]
    fn intentional_inactivity_and_dtx_sequence_continuity_do_not_estimate_loss() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in 0..3 {
            j.push_at(seq, bytes::Bytes::from_static(b"p"), now);
        }
        for _ in 0..3 {
            j.pop_at(true, now);
        }
        for _ in 0..20 {
            j.pop_at(true, now + Duration::from_secs(1));
        }
        // DTX may skip RTP timestamps, not RTP sequence numbers. Timestamps
        // aren't used to infer loss; empty-tail concealment isn't loss either.
        j.push_at(
            3,
            bytes::Bytes::from_static(b"p"),
            now + Duration::from_secs(2),
        );
        assert!(
            j.pop_at(true, now + Duration::from_millis(2060))
                .unwrap()
                .is_some()
        );
        assert_eq!(j.metrics.snapshot().total.sequence_gap_packets, 0);
    }
    #[test]
    fn reorders_deduplicates_and_wraps() {
        let mut j = Jitter::default();
        for i in [65534, 0, 65535, 65535] {
            j.push(i, bytes::Bytes::from(i.to_string()));
        }
        for i in [65534, 65535, 0] {
            assert_eq!(j.pop(true).unwrap().unwrap(), i.to_string());
        }
        assert!(j.pop(true).unwrap().is_none());
        j.push(65535, bytes::Bytes::from_static(b"late"));
        assert!(j.pop(true).unwrap().is_none());
    }
    #[test]
    fn pcm_prefetch_does_not_conceal_a_packet_before_its_deadline() {
        let mut j = Jitter::default();
        for seq in [0, 2, 3] {
            j.push(seq, bytes::Bytes::from_static(b"packet"));
        }
        assert!(j.pop(false).unwrap().is_some());
        for _ in 0..5 {
            assert!(j.pop(false).is_none());
        }
        j.push(1, bytes::Bytes::from_static(b"late but playable"));
        assert_eq!(j.pop(false).unwrap().unwrap(), "late but playable");
        assert!(j.pop(false).unwrap().is_some());
        assert!(j.pop(false).unwrap().is_some());
        assert!(j.pop(false).is_none());
        assert!(j.pop(true).unwrap().is_none());
    }
}
