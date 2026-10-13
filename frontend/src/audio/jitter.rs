//! Bounded 48 kHz/20 ms playout. Timestamps measure media; sequences reject
//! duplicates and packets behind already consumed audio. Clocks are injectable.
use super::{frames::sample_duration, mixer::FRAME};
use std::time::{Duration, Instant};

pub const PACKETS: usize = 12;
pub const MAX_PACKETS: usize = PACKETS;
pub const MAX_AGE: Duration = Duration::from_millis(200);
const PREBUFFER: Duration = Duration::from_millis(60);
const MAX_GAP: u32 = 3 * FRAME as u32;
const WINDOW: i32 = PACKETS as i32 * FRAME as i32;

pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub arrived: Instant,
    pub payload: bytes::Bytes,
}
#[derive(Debug, PartialEq)]
pub enum Playout {
    Packet(bytes::Bytes),
    /// Borrow the following packet; its normal decode remains due.
    Fec(bytes::Bytes),
    /// Full packets use Opus PLC; fractional holes use exact-length silence.
    Conceal(usize),
}
#[derive(Default, Clone, serde::Serialize)]
pub struct Metrics {
    pub target_ms: f64,
    /// Arrival-to-render estimate, excluding the OS/device buffer.
    pub current_ms: f64,
    pub variation_ms: f64,
    pub packets: usize,
    pub pcm_samples: usize,
    pub late_discards: u64,
    pub duplicates: u64,
    pub capacity_discards: u64,
    pub rebufferings: u64,
    pub discontinuities: u64,
    pub worker_stalls: u64,
    pub worker_queue_discards: u64,
    pub stale_discards: u64,
    pub concealments: u64,
    pub fec_attempts: u64,
    pub decode_errors: u64,
    pub inactive: bool,
    pub drift_ppm: f64,
    pub correction_ppm: f64,
}

pub type Decode = Playout;
pub struct Scheduled {
    pub decode: Playout,
    pub reset: bool,
    pub correction_ppm: f64,
}

#[cfg(test)]
#[path = "jitter/tests.rs"]
mod adaptive_tests;
#[cfg(test)]
pub(super) mod legacy;

#[derive(Default)]
struct Counters {
    rejected: u64,
    expired: u64,
    overflow: u64,
    resets: u64,
    concealed_samples: u64,
}
pub struct Jitter {
    packets: Vec<Packet>,
    cursor: Option<u32>,
    floor: Option<u32>,
    last_sequence: Option<u16>,
    first: Option<Instant>,
    play_at: Option<Instant>,
    playing: bool,
    missing: u32,
    reset_pending: bool,
    counters: Counters,
    adaptive: bool,
    latest: Option<(u32, Instant)>,
    last_tick: Option<Instant>,
    last_service: Option<Instant>,
    last_raise: Option<Instant>,
    last_rebuffer: Option<Instant>,
    late_at_rebuffer: u64,
    filtered_error: f64,
    reset_rate: bool,
    seen: [Option<(u16, u32)>; 32],
    seen_index: usize,
    metrics: Metrics,
}
impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: Vec::with_capacity(PACKETS),
            cursor: None,
            floor: None,
            last_sequence: None,
            first: None,
            play_at: None,
            playing: false,
            missing: 0,
            reset_pending: false,
            counters: Default::default(),
            adaptive: false,
            latest: None,
            last_tick: None,
            last_service: None,
            last_raise: None,
            last_rebuffer: None,
            late_at_rebuffer: 0,
            filtered_error: 0.0,
            reset_rate: true,
            seen: [None; 32],
            seen_index: 0,
            metrics: Metrics {
                target_ms: 40.0,
                inactive: true,
                ..Default::default()
            },
        }
    }
}
impl Jitter {
    pub fn push(
        &mut self,
        sequence: u16,
        timestamp: u32,
        payload: bytes::Bytes,
        received: Instant,
        now: Instant,
    ) {
        if now.saturating_duration_since(received) >= MAX_AGE {
            self.counters.expired += 1;
            self.metrics.worker_queue_discards += 1;
            return;
        }
        if !payload.is_empty() && payload.len() <= 1500 {
            self.observe(sequence, timestamp, received);
        }
        if payload.is_empty()
            || payload.len() > 1500
            || self
                .floor
                .is_some_and(|floor| (timestamp.wrapping_sub(floor) as i32) < 0)
            || self.last_sequence.is_some_and(|last| {
                sequence.wrapping_sub(last) as i16 <= 0
                    && !self
                        .floor
                        .is_some_and(|floor| timestamp.wrapping_sub(floor) as i32 > WINDOW)
            })
            || self.packets.iter().any(|p| {
                let seq = sequence.wrapping_sub(p.sequence) as i16;
                let time = timestamp.wrapping_sub(p.timestamp) as i32;
                seq == 0
                    || time.unsigned_abs() < FRAME as u32
                    || i32::from(seq.signum()) != time.signum()
            })
        {
            self.counters.rejected += 1;
            if self
                .floor
                .is_some_and(|floor| (timestamp.wrapping_sub(floor) as i32) < 0)
            {
                self.metrics.late_discards += 1;
            }
            return;
        }
        if let Some(cursor) = self.cursor {
            let distance = timestamp.wrapping_sub(cursor) as i32;
            if distance < 0 {
                // Reordering can move the initial anchor only before playout.
                if self.playing || self.last_sequence.is_some() || distance < -WINDOW {
                    self.counters.rejected += 1;
                    return;
                }
                self.cursor = Some(timestamp);
                self.play_at = self
                    .play_at
                    .and_then(|at| at.checked_sub(sample_duration((-distance) as usize)));
            }
        } else {
            self.cursor = Some(timestamp);
            self.first = Some(received);
            self.play_at = Some(received + self.prebuffer());
        }
        if self.packets.len() == PACKETS {
            // Keep the live edge without allocating beyond the packet cap.
            let cursor = self.cursor.unwrap();
            let oldest = self
                .packets
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.timestamp.wrapping_sub(cursor) as i32)
                .unwrap()
                .0;
            if timestamp.wrapping_sub(self.packets[oldest].timestamp) as i32 <= 0 {
                self.counters.overflow += 1;
                return;
            }
            self.packets.swap_remove(oldest);
            self.counters.overflow += 1;
            self.reset_pending = true;
        }
        self.packets.push(Packet {
            sequence,
            timestamp,
            arrived: received,
            payload,
        });
    }

    /// Call even when PCM is full. True requires a decoder and callback-ring
    /// reset before pop: a stalled output must not retain old decoded speech.
    pub fn maintain(&mut self, now: Instant) -> bool {
        let before = self.packets.len();
        self.packets
            .retain(|p| now.saturating_duration_since(p.arrived) < MAX_AGE);
        self.counters.expired += (before - self.packets.len()) as u64;
        let far = self.cursor.is_some_and(|cursor| {
            self.packets
                .iter()
                .map(|p| p.timestamp.wrapping_sub(cursor) as i32)
                .min()
                .is_some_and(|gap| gap > MAX_GAP as i32 || (gap > 0 && self.missing >= MAX_GAP))
        });
        // The fixed anchor cannot detect lag during rate-corrected playback:
        // sender/output drift deliberately moves it relative to wall time.
        // Expire remaining PCM even when all packets have already been decoded.
        let lagging = if self.adaptive {
            self.playing
                && self
                    .latest
                    .is_some_and(|(_, at)| now.saturating_duration_since(at) >= MAX_AGE)
        } else {
            self.play_at
                .is_some_and(|at| now.saturating_duration_since(at) >= MAX_AGE)
        };
        let spread = self.cursor.is_some_and(|cursor| {
            self.packets
                .iter()
                .any(|p| p.timestamp.wrapping_sub(cursor) as i32 > WINDOW)
        });
        if !self.reset_pending && before == self.packets.len() && !far && !lagging && !spread {
            return false;
        }
        self.counters.resets += 1;
        self.metrics.rebufferings += 1;
        self.metrics.discontinuities += 1;
        self.reset_rate = true;
        self.filtered_error = 0.0;
        self.metrics.correction_ppm = 0.0;
        self.metrics.drift_ppm = 0.0;
        self.reset_pending = false;
        self.playing = false;
        self.missing = 0;
        // Rebuffer only the newest 40 ms (up to three packets).
        if let Some(cursor) = self.cursor
            && let Some(latest) = self
                .packets
                .iter()
                .max_by_key(|p| p.timestamp.wrapping_sub(cursor) as i32)
                .map(|p| p.timestamp)
        {
            self.packets
                .retain(|p| latest.wrapping_sub(p.timestamp) <= 2 * FRAME as u32);
            let first = self
                .packets
                .iter()
                .min_by_key(|p| p.timestamp.wrapping_sub(cursor) as i32)
                .unwrap();
            self.cursor = Some(first.timestamp);
            self.floor = self.cursor;
            self.last_sequence = Some(first.sequence.wrapping_sub(1));
            self.first = Some(now);
            self.play_at = Some(now + self.prebuffer());
        } else {
            self.cursor = None;
            self.first = None;
            self.play_at = None;
        }
        true
    }
    pub fn pop(&mut self, conceal_missing: bool, now: Instant) -> Option<Playout> {
        let cursor = self.cursor?;
        if !self.playing {
            if (self.adaptive && now < self.play_at?)
                || (!self.adaptive
                    && self.packets.len() < 3
                    && now.saturating_duration_since(self.first?) < PREBUFFER)
            {
                return None;
            }
            self.playing = true;
        }
        if let Some(index) = self.packets.iter().position(|p| p.timestamp == cursor) {
            let packet = self.packets.swap_remove(index);
            self.last_sequence = Some(packet.sequence);
            self.missing = 0;
            self.advance(FRAME);
            return Some(Playout::Packet(packet.payload));
        }
        if !conceal_missing || self.missing >= MAX_GAP {
            return None;
        }
        let gap = self
            .packets
            .iter()
            .map(|p| p.timestamp.wrapping_sub(cursor))
            .min()
            .unwrap_or(FRAME as u32)
            .min(FRAME as u32)
            .min(MAX_GAP - self.missing);
        // FEC describes the preceding encoded 20 ms interval. Require both an
        // exact media successor and a sequence hole: consecutive sequences with
        // a timestamp gap can be capture discontinuities, with no such interval.
        let fec = (gap == FRAME as u32)
            .then(|| {
                self.packets
                    .iter()
                    .find(|p| {
                        p.timestamp == cursor.wrapping_add(FRAME as u32)
                            && self
                                .last_sequence
                                .is_some_and(|last| p.sequence.wrapping_sub(last) as i16 > 1)
                    })
                    .map(|p| p.payload.clone())
            })
            .flatten();
        self.missing += gap;
        self.counters.concealed_samples += u64::from(gap);
        self.advance(gap as usize);
        Some(fec.map_or(Playout::Conceal(gap as usize), Playout::Fec))
    }
    fn advance(&mut self, samples: usize) {
        self.cursor = self.cursor.map(|v| v.wrapping_add(samples as u32));
        self.floor = self.cursor;
        self.play_at = self.play_at.map(|at| at + sample_duration(samples));
        self.packets
            .retain(|p| p.timestamp.wrapping_sub(self.cursor.unwrap()) as i32 >= 0);
    }
    /// Deafen/device changes keep sequence/media watermarks, unlike a new source.
    pub fn discard(&mut self) {
        if let Some(cursor) = self.cursor
            && let Some(latest) = self
                .packets
                .iter()
                .max_by_key(|p| p.timestamp.wrapping_sub(cursor) as i32)
        {
            self.floor = Some(latest.timestamp.wrapping_add(FRAME as u32));
            self.last_sequence = Some(latest.sequence);
        }
        self.packets.clear();
        self.cursor = None;
        self.first = None;
        self.play_at = None;
        self.playing = false;
        self.missing = 0;
        self.reset_pending = false;
        self.latest = None;
        self.last_tick = None;
        self.last_service = None;
        self.reset_rate = true;
        self.filtered_error = 0.0;
        self.metrics.drift_ppm = 0.0;
        self.metrics.correction_ppm = 0.0;
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"rejected":self.counters.rejected,"expired":self.counters.expired,
            "overflow":self.counters.overflow,"resets":self.counters.resets,
            "concealed_samples":self.counters.concealed_samples})
    }

    fn prebuffer(&self) -> Duration {
        Duration::from_secs_f64(self.metrics.target_ms / 1000.0)
    }
    pub fn push_packet(&mut self, packet: Packet, now: Instant) {
        self.push(
            packet.sequence,
            packet.timestamp,
            packet.payload,
            packet.arrived,
            now,
        );
    }
    fn observe(&mut self, sequence: u16, timestamp: u32, arrived: Instant) {
        let identity = (sequence, timestamp);
        if self.seen.contains(&Some(identity)) {
            self.metrics.duplicates += 1;
            return;
        }
        self.seen[self.seen_index] = Some(identity);
        self.seen_index = (self.seen_index + 1) % self.seen.len();
        if let Some((ts, at)) = self.latest {
            let media = timestamp.wrapping_sub(ts) as i32;
            let elapsed = arrived.saturating_duration_since(at).as_secs_f64();
            if arrived >= at && media.unsigned_abs() <= 48000 && elapsed <= 1.0 {
                let variation = (elapsed * 1000.0 - f64::from(media) / 48.0)
                    .abs()
                    .min(120.0);
                self.metrics.variation_ms += (variation - self.metrics.variation_ms) / 16.0;
                let wanted = (40.0 + 4.0 * self.metrics.variation_ms).clamp(40.0, 120.0);
                if wanted > self.metrics.target_ms + 5.0 {
                    self.metrics.target_ms = wanted;
                    self.last_raise = Some(arrived);
                } else if media > 0
                    && wanted < self.metrics.target_ms - 5.0
                    && self
                        .last_raise
                        .is_none_or(|t| arrived.saturating_duration_since(t).as_secs() >= 5)
                {
                    self.metrics.target_ms = (self.metrics.target_ms - elapsed).max(40.0);
                }
            }
            if media > 0 {
                self.latest = Some((timestamp, arrived));
            }
        } else {
            self.latest = Some((timestamp, arrived));
        }
    }
    /// Maintenance runs even with a full ring. The caller must flush and await
    /// callback acknowledgement before scheduling PCM after a hard reset.
    pub fn prepare(&mut self, now: Instant) -> bool {
        self.adaptive = true;
        if self
            .last_service
            .replace(now)
            .is_some_and(|at| now.saturating_duration_since(at) > Duration::from_millis(100))
            && self.playing
        {
            self.reset_pending = true;
            self.metrics.worker_stalls += 1;
        }
        self.maintain(now)
    }
    pub fn metrics(&self) -> Metrics {
        Metrics {
            packets: self.packets.len(),
            capacity_discards: self.counters.overflow,
            stale_discards: self
                .counters
                .expired
                .saturating_sub(self.metrics.worker_queue_discards),
            inactive: self.cursor.is_none() || (self.packets.is_empty() && self.missing >= MAX_GAP),
            ..self.metrics.clone()
        }
    }
    pub fn decode_failed(&mut self) {
        self.metrics.decode_errors += 1;
    }
    #[cfg(test)]
    fn tick(&mut self, now: Instant, pcm_samples: usize) -> Option<Scheduled> {
        self.prepare(now);
        self.poll(now, pcm_samples)
    }
    /// The worker must call prepare once before its bounded decode loop and
    /// honor the flush acknowledgement. Never perform a hidden hard reset here.
    pub fn poll(&mut self, now: Instant, pcm_samples: usize) -> Option<Scheduled> {
        self.metrics.pcm_samples = pcm_samples;
        let elapsed = self
            .last_tick
            .replace(now)
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or_default();
        let timestamp = self.cursor?;
        if !self.playing && (now < self.play_at? || pcm_samples != 0) {
            return None;
        }
        if let Some((latest, arrived)) = self.latest {
            let age = now.saturating_duration_since(arrived).as_secs_f64() * 1000.0;
            self.metrics.current_ms = age
                + f64::from(latest.wrapping_sub(timestamp) as i32) / 48.0
                + pcm_samples as f64 / 48.0;
            let deficit = self.metrics.target_ms - self.metrics.current_ms;
            if deficit > 20.0
                && self.metrics.variation_ms > 5.0
                && self.metrics.late_discards > self.late_at_rebuffer
                && self
                    .last_rebuffer
                    .is_none_or(|t| now.saturating_duration_since(t).as_secs() >= 5)
            {
                // A sudden path change needs tolerance now, not after tens of
                // seconds of gentle rate correction. Drain existing PCM, then
                // insert one explicit bounded rebuffer interval. Never cut PCM.
                self.playing = false;
                self.play_at = Some(
                    now + Duration::from_secs_f64(
                        (deficit.min(80.0) + pcm_samples as f64 / 48.0) / 1000.0,
                    ),
                );
                self.last_rebuffer = Some(now);
                self.late_at_rebuffer = self.metrics.late_discards;
                self.metrics.rebufferings += 1;
                self.filtered_error = 0.0;
                self.metrics.correction_ppm = 0.0;
                return None;
            }
            if elapsed <= Duration::from_millis(100) && age < 200.0 && self.missing == 0 {
                let dt = elapsed.as_secs_f64();
                let error = self.metrics.current_ms - self.metrics.target_ms;
                self.filtered_error += dt / (5.0 + dt) * (error - self.filtered_error);
                // Slow integral learns sender/output clock skew. Fast delay
                // changes remain the proportional term, not the drift estimate.
                if self.metrics.variation_ms < 5.0
                    && error.abs() < 10.0
                    && self
                        .last_raise
                        .is_none_or(|at| now.saturating_duration_since(at).as_secs() >= 10)
                {
                    self.metrics.drift_ppm = (self.metrics.drift_ppm
                        + self.filtered_error * dt * 5.0)
                        .clamp(-1000.0, 1000.0);
                }
                let wanted =
                    (self.metrics.drift_ppm + self.filtered_error * 100.0).clamp(-5000.0, 5000.0);
                self.metrics.correction_ppm +=
                    (wanted - self.metrics.correction_ppm).clamp(-1000.0 * dt, 1000.0 * dt);
            }
        }

        if pcm_samples > 720 {
            return None;
        }
        let decode = self.pop(pcm_samples <= 240, now)?;
        match &decode {
            Playout::Fec(_) => {
                self.metrics.fec_attempts += 1;
                self.metrics.concealments += 1;
            }
            Playout::Conceal(_) => self.metrics.concealments += 1,
            Playout::Packet(_) => {}
        }
        Some(Scheduled {
            decode,
            reset: std::mem::take(&mut self.reset_rate),
            correction_ppm: self.metrics.correction_ppm,
        })
    }

    pub(super) fn reject(&mut self) {
        self.counters.rejected += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn push(j: &mut Jitter, seq: u16, ts: u32, now: Instant) {
        j.push(seq, ts, bytes::Bytes::from(seq.to_string()), now, now);
    }
    fn packet(j: &mut Jitter, seq: u16, now: Instant) {
        assert!(!j.maintain(now));
        match j.pop(true, now) {
            Some(Playout::Packet(bytes)) => assert_eq!(bytes, seq.to_string()),
            _ => panic!("expected real packet {seq}"),
        }
    }
    fn conceal(j: &mut Jitter, samples: usize, now: Instant) {
        assert!(!j.maintain(now));
        assert!(matches!(j.pop(true, now), Some(Playout::Conceal(n)) if n == samples));
    }

    #[test]
    fn reordering_duplicates_and_independent_wrap_do_not_rewind_playout() {
        let now = Instant::now();
        let start = u32::MAX - 479;
        let mut j = Jitter::default();
        for (seq, offset) in [(u16::MAX, 960), (0, 1920), (65534, 0), (65535, 960)] {
            push(&mut j, seq, start.wrapping_add(offset), now);
        }
        for seq in [65534, 65535, 0] {
            packet(&mut j, seq, now);
        }
        push(&mut j, 65535, start.wrapping_add(960), now);
        assert!(j.packets.is_empty());
        assert_eq!(j.counters.rejected, 2);
        assert_eq!(j.cursor, Some(start.wrapping_add(2880)));
    }

    #[test]
    fn sender_drops_have_media_holes_without_sequence_holes() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for (seq, ts) in [(0, 0), (1, 1920), (2, 4800)] {
            push(&mut j, seq, ts, now);
        }
        packet(&mut j, 0, now);
        assert!(j.pop(false, now).is_none());
        conceal(&mut j, 960, now);
        packet(&mut j, 1, now);
        conceal(&mut j, 960, now);
        conceal(&mut j, 960, now);
        packet(&mut j, 2, now);
        assert_eq!(j.counters.concealed_samples, 2880);
    }

    #[test]
    fn partial_capture_holes_preserve_exact_sample_count() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for (seq, ts) in [(0, 0), (1, 1097), (2, 2057)] {
            push(&mut j, seq, ts, now);
        }
        packet(&mut j, 0, now);
        conceal(&mut j, 137, now);
        packet(&mut j, 1, now);
        packet(&mut j, 2, now);
        assert_eq!(j.cursor, Some(3017));
    }

    #[test]
    fn fec_waits_for_deadline_and_borrows_successor_once_across_both_wraps() {
        let now = Instant::now();
        let mut j = Jitter::default();
        let start = u32::MAX - 479;
        for (seq, offset) in [(65534, 0), (0, 1920), (1, 2880)] {
            push(&mut j, seq, start.wrapping_add(offset), now);
        }
        packet(&mut j, 65534, now);
        for _ in 0..5 {
            assert!(j.pop(false, now).is_none());
        }
        assert_eq!(
            j.pop(true, now),
            Some(Playout::Fec(bytes::Bytes::from_static(b"0")))
        );
        push(&mut j, 65535, start.wrapping_add(960), now);
        push(&mut j, 0, start.wrapping_add(1920), now);
        packet(&mut j, 0, now);
        packet(&mut j, 1, now);
        assert_eq!(j.counters.rejected, 2);
        assert_eq!(j.counters.concealed_samples, 960);
    }

    #[test]
    fn burst_loss_uses_fec_only_immediately_before_successor_and_never_replays() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in [0, 3, 4] {
            push(&mut j, seq, u32::from(seq) * 960, now);
        }
        packet(&mut j, 0, now);
        conceal(&mut j, 960, now);
        assert_eq!(
            j.pop(true, now),
            Some(Playout::Fec(bytes::Bytes::from_static(b"3")))
        );
        packet(&mut j, 3, now);
        packet(&mut j, 4, now);
        conceal(&mut j, 960, now);
        push(&mut j, 6, 5760, now);
        packet(&mut j, 6, now);
    }

    #[test]
    fn a_fractional_media_gap_with_sequence_loss_does_not_borrow_fec() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for (seq, ts) in [(0, 0), (2, 1097), (3, 2057)] {
            push(&mut j, seq, ts, now);
        }
        packet(&mut j, 0, now);
        conceal(&mut j, 137, now);
        packet(&mut j, 2, now);
        packet(&mut j, 3, now);
    }

    #[test]
    fn network_loss_conceals_once_and_prefetch_waits_for_delayed_packets() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in [0, 2, 3] {
            push(&mut j, seq, u32::from(seq) * 960, now);
        }
        packet(&mut j, 0, now);
        for _ in 0..5 {
            assert!(j.pop(false, now).is_none());
        }
        push(&mut j, 1, 960, now);
        packet(&mut j, 1, now);
        packet(&mut j, 2, now);
        packet(&mut j, 3, now);
        push(&mut j, 5, 4800, now);
        assert_eq!(
            j.pop(true, now),
            Some(Playout::Fec(bytes::Bytes::from_static(b"5")))
        );
        packet(&mut j, 5, now);
        assert_eq!(j.counters.concealed_samples, 960);
    }

    #[test]
    fn long_timestamp_jump_rebuffers_with_no_unbounded_concealment() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in 0..3 {
            push(&mut j, seq, u32::from(seq) * 960, now);
        }
        for seq in 0..3 {
            packet(&mut j, seq, now);
        }
        push(&mut j, 3, 480_000, now);
        assert!(j.maintain(now));
        assert!(j.pop(true, now).is_none());
        packet(&mut j, 3, now + PREBUFFER);
        assert_eq!(j.counters.concealed_samples, 0);
        assert_eq!(j.counters.resets, 1);
        push(&mut j, 4, 3840, now + PREBUFFER);
        assert!(j.packets.is_empty());
    }

    #[test]
    fn clock_stall_and_queue_age_resume_at_live_edge() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in 0..3 {
            push(&mut j, seq, u32::from(seq) * 960, now);
        }
        packet(&mut j, 0, now);
        let resumed = now + Duration::from_secs(2);
        // Delivered through the worker queue after stalling: preserve arrival age.
        j.push(3, 2880, bytes::Bytes::from_static(b"old"), now, resumed);
        for seq in 4..7 {
            push(&mut j, seq, 96000 + u32::from(seq - 4) * 960, resumed);
        }
        assert!(j.maintain(resumed));
        packet(&mut j, 4, resumed);
        assert_eq!(j.counters.expired, 3);
        assert_eq!(j.counters.resets, 1);
    }

    #[test]
    fn no_packets_means_finite_plc_and_late_audio_stays_late_after_idle() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, 0, 0, now);
        let playing = now + PREBUFFER;
        packet(&mut j, 0, playing);
        for _ in 0..3 {
            conceal(&mut j, 960, playing);
        }
        for _ in 0..1000 {
            assert!(j.pop(true, playing).is_none());
        }
        let later = now + Duration::from_secs(2);
        assert!(j.maintain(later));
        push(&mut j, 1, 960, later);
        assert!(j.packets.is_empty());
        push(&mut j, 2, 96000, later);
        packet(&mut j, 2, later + PREBUFFER);
    }

    #[test]
    fn pressure_and_malformed_headers_have_bounded_storage_and_recovery() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in 0..10_000u16 {
            push(&mut j, seq, u32::from(seq) * 960, now);
            assert!(j.packets.len() <= PACKETS);
            assert_eq!(j.packets.capacity(), PACKETS);
        }
        assert!(j.maintain(now));
        assert_eq!(j.packets.len(), 3);
        for seq in 9997..10_000 {
            packet(&mut j, seq, now);
        }
        push(&mut j, 10_000, 960, now); // backward media with forward sequence
        j.push(
            10_001,
            9_600_960,
            bytes::Bytes::from(vec![0; 1501]),
            now,
            now,
        );
        assert!(j.packets.is_empty());
        assert_eq!(j.counters.overflow, 10000 - PACKETS as u64);
    }

    #[test]
    fn sequence_holes_do_not_insert_audio_if_media_time_is_contiguous() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for (seq, ts) in [(0, 0), (100, 960), (101, 1920)] {
            push(&mut j, seq, ts, now);
        }
        for seq in [0, 100, 101] {
            packet(&mut j, seq, now);
        }
        assert_eq!(j.counters.concealed_samples, 0);
    }

    #[test]
    fn long_idle_can_cross_the_sequence_half_range_without_losing_the_media_floor() {
        let now = Instant::now();
        let mut j = Jitter::default();
        for seq in 0..3 {
            push(&mut j, seq, u32::from(seq) * 960, now);
        }
        for seq in 0..3 {
            packet(&mut j, seq, now);
        }
        j.discard();
        let later = now + Duration::from_secs(800);
        push(&mut j, 40_000, 38_400_000, later);
        packet(&mut j, 40_000, later + PREBUFFER);
        push(&mut j, 40_001, 960, later + PREBUFFER);
        assert!(j.packets.is_empty());
    }

    #[test]
    fn a_mixed_old_and_live_burst_rebases_before_playing_its_old_prefix() {
        let now = Instant::now();
        let mut j = Jitter::default();
        push(&mut j, 0, 0, now);
        for seq in 1..4 {
            push(&mut j, seq, 48000 + u32::from(seq - 1) * 960, now);
        }
        assert!(j.maintain(now));
        for seq in 1..4 {
            packet(&mut j, seq, now);
        }
        assert_eq!(j.counters.concealed_samples, 0);
    }
}
