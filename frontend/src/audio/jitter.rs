use super::mixer::FRAME;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

struct Packet {
    sequence: u16,
    marker: bool,
    payload: bytes::Bytes,
}
pub enum Playout {
    Packet(bytes::Bytes),
    Conceal,
    /// Contiguous sequence with a media-time gap proves intentional omission.
    Silence,
    /// Without a following packet, silence and an outage are indistinguishable.
    Inactive,
}

/// Fixed 60 ms startup, at most eight 20 ms packets. PCM consumption drives the
/// media cursor, including silence. Missing sequence numbers, not empty media
/// slots, count as loss. A connection/roster change replaces this entire state.
pub struct Jitter {
    packets: HashMap<u32, Packet>,
    cursor: Option<u32>,
    last: Option<(u16, u32)>,
    first: Instant,
    playing: bool,
    missing: u8,
    silence: bool,
    recent_loss: VecDeque<(u16, u32)>,
    pub lost_packets: u64,
    pub late_packets: u64,
    pub omitted_frames: u64,
    pub resyncs: u64,
}
impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: HashMap::with_capacity(8),
            cursor: None,
            last: None,
            first: Instant::now(),
            playing: false,
            missing: 0,
            silence: false,
            recent_loss: VecDeque::with_capacity(64),
            lost_packets: 0,
            late_packets: 0,
            omitted_frames: 0,
            resyncs: 0,
        }
    }
}
impl Jitter {
    pub fn push(
        &mut self,
        sequence: u16,
        timestamp: u32,
        marker: bool,
        payload: bytes::Bytes,
        now: Instant,
    ) {
        if payload.is_empty() || payload.len() > 1500 {
            return;
        }
        if let Some(index) = self
            .recent_loss
            .iter()
            .position(|(seq, _)| *seq == sequence)
        {
            self.recent_loss.remove(index);
            self.lost_packets = self.lost_packets.saturating_sub(1);
            self.late_packets += 1;
            return;
        }
        if self.last.is_some_and(|(seq, ts)| {
            sequence.wrapping_sub(seq) as i16 <= 0 || timestamp.wrapping_sub(ts) as i32 <= 0
        }) {
            return;
        }
        let cursor = *self.cursor.get_or_insert_with(|| {
            self.first = now;
            timestamp
        });
        let distance = timestamp.wrapping_sub(cursor) as i32;
        let resuming = self.missing >= 3
            && self
                .last
                .is_some_and(|(_, ts)| timestamp.wrapping_sub(ts) > (FRAME * 10) as u32);
        if distance < 0 && !resuming {
            self.late_packets += 1;
            self.observe(sequence, timestamp, marker);
            return;
        }
        // Catch up after suspension without replaying seconds of old silence or
        // imposing another three-packet startup requirement.
        if distance > (FRAME * 50) as i32
            || (distance < 0 && resuming)
            || distance % FRAME as i32 != 0
        {
            self.packets.clear();
            self.cursor = Some(timestamp);
            self.resyncs += 1;
        }
        if self
            .packets
            .values()
            .any(|packet| packet.sequence == sequence)
        {
            return;
        }
        if self.packets.len() < 8 {
            self.packets.entry(timestamp).or_insert(Packet {
                sequence,
                marker,
                payload,
            });
        }
    }
    fn observe(&mut self, sequence: u16, timestamp: u32, marker: bool) {
        self.recent_loss
            .retain(|(_, at)| timestamp.wrapping_sub(*at) <= 2 * 48_000);
        if let Some((last_seq, last_ts)) = self.last {
            let sequences = sequence.wrapping_sub(last_seq) as u32;
            let frames = timestamp.wrapping_sub(last_ts) / FRAME as u32;
            self.lost_packets += sequences.saturating_sub(1) as u64;
            // Reclassify subsequently arriving late packets, without rewinding
            // the decoder. Work and history are capped even for large jumps.
            for back in (1..sequences.min(65)).rev() {
                if self.recent_loss.len() == 64 {
                    self.recent_loss.pop_front();
                }
                self.recent_loss
                    .push_back((sequence.wrapping_sub(back as u16), timestamp));
            }
            // With loss, only a lower bound on omitted frames is knowable.
            self.omitted_frames += frames.saturating_sub(sequences) as u64;
            self.silence = !marker && frames > sequences;
        }
        self.last = Some((sequence, timestamp));
    }
    pub fn quiet(&self) -> bool {
        !self.playing || self.silence || self.missing >= 3
    }
    pub fn pop(&mut self, conceal_missing: bool, now: Instant) -> Option<Playout> {
        let cursor = self.cursor?;
        if !self.playing {
            if self.packets.len() < 3
                && now.saturating_duration_since(self.first) < Duration::from_millis(60)
            {
                return None;
            }
            self.playing = true;
        }
        let result = if let Some(packet) = self.packets.remove(&cursor) {
            self.observe(packet.sequence, cursor, packet.marker);
            self.missing = 0;
            Playout::Packet(packet.payload)
        } else {
            let intentional = self.last.is_some_and(|(seq, _)| {
                self.packets
                    .values()
                    .any(|p| p.sequence == seq.wrapping_add(1))
            });
            if !conceal_missing && !intentional && !self.quiet() {
                return None;
            }
            self.missing = self.missing.saturating_add(1);
            if intentional || (self.silence && self.missing <= 50) {
                Playout::Silence
            } else if self.missing <= 3 {
                Playout::Conceal
            } else {
                Playout::Inactive
            }
        };
        self.cursor = Some(cursor.wrapping_add(FRAME as u32));
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn push(j: &mut Jitter, seq: u16, ts: u32, now: Instant) {
        j.push(seq, ts, false, bytes::Bytes::from_static(b"packet"), now);
    }
    #[test]
    fn reorders_deduplicates_and_wraps_both_rtp_clocks() {
        let mut j = Jitter::default();
        let now = Instant::now();
        let base = u32::MAX - 959;
        for i in [0u16, 2, 1, 1] {
            push(
                &mut j,
                65534u16.wrapping_add(i),
                base.wrapping_add(i as u32 * 960),
                now,
            );
        }
        for _ in 0..3 {
            assert!(matches!(j.pop(true, now), Some(Playout::Packet(_))));
        }
        assert!(matches!(j.pop(true, now), Some(Playout::Conceal)));
        push(&mut j, 65535, base, now);
        assert_eq!(j.lost_packets, 0);
        assert!(j.packets.is_empty());
    }
    #[test]
    fn pcm_prefetch_waits_for_the_missing_packets_deadline() {
        let mut j = Jitter::default();
        let now = Instant::now();
        for seq in [0, 2, 3] {
            push(&mut j, seq, seq as u32 * 960, now);
        }
        assert!(matches!(j.pop(false, now), Some(Playout::Packet(_))));
        for _ in 0..5 {
            assert!(j.pop(false, now).is_none());
        }
        push(&mut j, 1, 960, now);
        for _ in 0..3 {
            assert!(matches!(j.pop(false, now), Some(Playout::Packet(_))));
        }
        assert_eq!(j.lost_packets, 0);
    }
    #[test]
    fn silence_is_not_sequence_loss_and_does_not_rebuffer() {
        let mut j = Jitter::default();
        let now = Instant::now();
        push(&mut j, 1, 0, now);
        push(&mut j, 2, 20 * 960, now);
        push(&mut j, 3, 40 * 960, now);
        assert!(matches!(j.pop(true, now), Some(Playout::Packet(_))));
        for _ in 1..20 {
            assert!(matches!(j.pop(false, now), Some(Playout::Silence)));
        }
        assert!(matches!(j.pop(true, now), Some(Playout::Packet(_))));
        for _ in 21..40 {
            assert!(matches!(j.pop(false, now), Some(Playout::Silence)));
        }
        assert!(matches!(j.pop(true, now), Some(Playout::Packet(_))));
        for _ in 41..3000 {
            assert!(matches!(
                j.pop(true, now),
                Some(Playout::Silence | Playout::Inactive)
            ));
        }
        j.push(
            5,
            3000 * 960,
            true,
            bytes::Bytes::from_static(b"speech"),
            now,
        );
        assert!(matches!(j.pop(true, now), Some(Playout::Packet(_))));
        assert_eq!(j.lost_packets, 1);
        assert!(!j.quiet());
    }
    #[test]
    fn unknown_outage_bounds_concealment_and_tracks_late_separately() {
        let mut j = Jitter::default();
        let now = Instant::now();
        push(&mut j, 0, 0, now);
        let ready = now + Duration::from_millis(60);
        assert!(matches!(j.pop(true, ready), Some(Playout::Packet(_))));
        for _ in 0..3 {
            assert!(matches!(j.pop(true, ready), Some(Playout::Conceal)));
        }
        for _ in 0..100 {
            assert!(matches!(j.pop(true, ready), Some(Playout::Inactive)));
        }
        push(&mut j, 1, 960, ready);
        assert_eq!(j.late_packets, 1);
        assert_eq!(j.lost_packets, 0);
    }
    #[test]
    fn capture_half_frame_gap_and_late_resumption_realign_without_rebuffering() {
        let mut j = Jitter::default();
        let now = Instant::now();
        push(&mut j, 1, 0, now);
        let ready = now + Duration::from_millis(60);
        assert!(matches!(j.pop(true, ready), Some(Playout::Packet(_))));
        push(&mut j, 2, 1440, ready); // a lost 10 ms capture block
        assert!(matches!(j.pop(true, ready), Some(Playout::Packet(_))));
        for _ in 0..100 {
            j.pop(true, ready);
        }
        push(&mut j, 3, 50 * 960 + 1440, ready);
        assert!(matches!(j.pop(true, ready), Some(Playout::Packet(_))));
        assert_eq!(j.resyncs, 2);
        assert_eq!(j.lost_packets, 0);
    }
    #[test]
    fn reordered_late_packets_are_not_both_loss_and_late() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in [0, 1, 4] {
            push(&mut j, seq, seq as u32 * 960, now);
        }
        for _ in 0..5 {
            j.pop(true, now);
        }
        assert_eq!(j.lost_packets, 2);
        for seq in [3, 2, 2] {
            push(&mut j, seq, seq as u32 * 960, now);
        }
        assert_eq!(j.lost_packets, 0);
        assert_eq!(j.late_packets, 2);
        assert!(j.packets.is_empty());
    }
}
