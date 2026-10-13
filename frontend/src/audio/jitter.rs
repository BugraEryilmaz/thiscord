//! Per-source media timeline. All clocks/PCM occupancy are supplied by the caller;
//! arrival time is captured by the receiver, never reconstructed on the worker.
use std::time::{Duration, Instant};

use super::mixer::FRAME;

#[cfg(test)]
pub(super) mod legacy;
#[cfg(test)]
mod tests;

pub const MAX_PACKETS: usize = 12;
const MIN_DELAY: f64 = 40.0;
const MAX_DELAY: f64 = 120.0;
// 15 ms covers the existing irregular 12 ms worker-wake regression. A decoded
// packet tops this up to at most 35.125 ms, all within the combined delay budget.
const PREFETCH: usize = 720;
const DEADLINE: usize = 240;

pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub arrived: Instant,
    pub payload: bytes::Bytes,
}

pub enum Decode {
    Packet(bytes::Bytes),
    /// Decode the following packet with Opus FEC; retain it for normal decoding.
    Fec(bytes::Bytes),
    Plc,
}

pub struct Playout {
    pub decode: Decode,
    pub reset: bool,
    pub correction_ppm: f64,
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

/// Packet ordering is bounded independently of startup and playout. Signed wrap
/// comparisons are valid within this short window (not across half an RTP epoch).
pub struct Jitter {
    packets: Vec<Packet>,
    expected: Option<(u16, u32)>,
    latest: Option<(u32, Instant)>,
    highest_sequence: Option<u16>,
    seen: [Option<(u16, u32)>; 32],
    seen_index: usize,
    start: Option<Instant>,
    last_tick: Option<Instant>,
    last_raise: Option<Instant>,
    last_rebuffer: Option<Instant>,
    late_at_rebuffer: u64,
    playing: bool,
    reset: bool,
    missing: u8,
    filtered_error: f64,
    metrics: Metrics,
}

impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: Vec::with_capacity(MAX_PACKETS),
            expected: None,
            latest: None,
            highest_sequence: None,
            seen: [None; 32],
            seen_index: 0,
            start: None,
            last_tick: None,
            last_raise: None,
            last_rebuffer: None,
            late_at_rebuffer: 0,
            playing: false,
            reset: true,
            missing: 0,
            filtered_error: 0.0,
            metrics: Metrics {
                target_ms: MIN_DELAY,
                inactive: true,
                ..Default::default()
            },
        }
    }
}

impl Jitter {
    pub fn metrics(&self) -> Metrics {
        Metrics {
            packets: self.packets.len(),
            ..self.metrics.clone()
        }
    }

    pub fn decode_failed(&mut self) {
        self.metrics.decode_errors += 1;
    }

    fn anchor(&mut self, sequence: u16, timestamp: u32, arrived: Instant) {
        self.expected = Some((sequence, timestamp));
        self.start = Some(arrived + Duration::from_secs_f64(self.metrics.target_ms / 1000.0));
        self.playing = false;
        self.reset = true;
        self.missing = 0;
        self.latest = None;
        self.filtered_error = 0.0;
        self.metrics.drift_ppm = 0.0;
        self.metrics.correction_ppm = 0.0;
        self.metrics.inactive = false;
    }

    pub fn push(&mut self, packet: Packet, now: Instant) {
        if now.saturating_duration_since(packet.arrived) > Duration::from_millis(160) {
            self.metrics.worker_queue_discards += 1;
            return;
        }
        if packet.payload.is_empty() || packet.payload.len() > 1500 {
            return;
        }
        let identity = (packet.sequence, packet.timestamp);
        if self.seen.contains(&Some(identity)) {
            self.metrics.duplicates += 1;
            return;
        }
        self.seen[self.seen_index] = Some(identity);
        self.seen_index = (self.seen_index + 1) % self.seen.len();
        // PLC advanced the tentative loss cursor, not the received watermark.
        // A sender that paused sequence numbering during DTX may resume behind
        // that cursor, but must still be newer than every received packet.
        if self.metrics.inactive
            && self
                .highest_sequence
                .is_some_and(|seq| packet.sequence.wrapping_sub(seq) as i16 > 0)
        {
            self.packets.clear();
            self.anchor(packet.sequence, packet.timestamp, packet.arrived);
        }
        if let Some((sequence, timestamp)) = self.expected {
            let distance = packet.sequence.wrapping_sub(sequence) as i16;
            if distance < 0 {
                // Late arrivals still contain network timing evidence. Measuring
                // only admitted packets would hide precisely the jitter that
                // exceeded our tolerance. Never move the latest media watermark.
                if let Some((ts, at)) = self.latest
                    && packet.arrived >= at
                {
                    let variation = (packet.arrived.duration_since(at).as_secs_f64() * 1000.0
                        - f64::from(packet.timestamp.wrapping_sub(ts) as i32) / 48.0)
                        .abs();
                    self.metrics.variation_ms +=
                        (variation.min(120.0) - self.metrics.variation_ms) / 16.0;
                    let wanted =
                        (MIN_DELAY + 4.0 * self.metrics.variation_ms).clamp(MIN_DELAY, MAX_DELAY);
                    if wanted > self.metrics.target_ms + 5.0 {
                        self.metrics.target_ms = wanted;
                        self.last_raise = Some(packet.arrived);
                    }
                }
                self.metrics.late_discards += 1;
                return;
            }
            if self.packets.iter().any(|p| p.sequence == packet.sequence) {
                self.metrics.duplicates += 1;
                return;
            }
            let media = packet.timestamp.wrapping_sub(timestamp) as i32;
            // A forward source jump or explicit timestamp gap is a new talkspurt,
            // not a request to synthesize hundreds of missing packets. Reordered
            // packets inside the window cannot rewind the media clock.
            if distance > 64
                || media > (distance as i32 + 10) * FRAME as i32
                || (distance == 0 && media != 0)
                || self.metrics.inactive
            {
                self.packets.clear();
                self.metrics.discontinuities += 1;
                self.anchor(packet.sequence, packet.timestamp, packet.arrived);
            } else if media < 0 {
                self.metrics.late_discards += 1;
                return;
            }
        } else {
            self.anchor(packet.sequence, packet.timestamp, packet.arrived);
        }
        if self.packets.len() == MAX_PACKETS {
            self.metrics.capacity_discards += 1;
            return;
        }
        if let Some((timestamp, arrived)) = self.latest {
            let media = packet.timestamp.wrapping_sub(timestamp) as i32;
            if media > 0 && packet.arrived >= arrived {
                let elapsed = packet.arrived.duration_since(arrived).as_secs_f64();
                let variation = (elapsed * 1000.0 - f64::from(media) / 48.0).abs();
                // Exclude long inactivity and discontinuities from the estimator.
                if media <= 48000 && elapsed <= 1.0 {
                    self.metrics.variation_ms += (variation - self.metrics.variation_ms) / 16.0;
                    let wanted =
                        (MIN_DELAY + 4.0 * self.metrics.variation_ms).clamp(MIN_DELAY, MAX_DELAY);
                    if wanted > self.metrics.target_ms + 5.0 {
                        self.metrics.target_ms = wanted;
                        self.last_raise = Some(packet.arrived);
                    } else if wanted < self.metrics.target_ms - 5.0
                        && self.last_raise.is_none_or(|t| {
                            packet.arrived.saturating_duration_since(t).as_secs() >= 5
                        })
                    {
                        // At most 1 ms/s, with a five-second hold after an increase.
                        self.metrics.target_ms = (self.metrics.target_ms - elapsed).max(MIN_DELAY);
                    }
                }
                self.latest = Some((packet.timestamp, packet.arrived));
            }
        } else {
            self.latest = Some((packet.timestamp, packet.arrived));
        }
        self.packets.push(packet);
        let seq = self.packets.last().expect("just inserted").sequence;
        if self
            .highest_sequence
            .is_none_or(|previous| seq.wrapping_sub(previous) as i16 > 0)
        {
            self.highest_sequence = Some(seq);
        }
    }

    /// PCM consumption supplies the output clock. A missing frame is resolved
    /// only within 5 ms of its actual ring deadline, even during real-packet
    /// prefetch. At most one decision is returned; callers cap work at two/tick.
    pub fn poll(&mut self, now: Instant, pcm_samples: usize) -> Option<Playout> {
        self.metrics.pcm_samples = pcm_samples;
        let elapsed = self
            .last_tick
            .replace(now)
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default();
        if elapsed > Duration::from_millis(100) && self.playing {
            self.metrics.worker_stalls += 1;
            self.rebuffer(now);
        }
        let (sequence, timestamp) = self.expected?;
        if self.metrics.inactive {
            return None;
        }
        if !self.playing {
            if now < self.start? || pcm_samples != 0 {
                return None;
            }
            self.playing = true;
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
                self.start = Some(
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
        if pcm_samples > PREFETCH {
            return None;
        }
        if let Some(index) = self.packets.iter().position(|p| p.sequence == sequence) {
            if now.saturating_duration_since(self.packets[index].arrived)
                > Duration::from_millis(200)
            {
                self.rebuffer(now);
                return None;
            }
            if self.packets[index].timestamp != timestamp {
                self.metrics.discontinuities += 1;
                self.rebuffer(now);
                return None;
            }
            let packet = self.packets.remove(index);
            self.missing = 0;
            self.advance();
            return Some(self.decision(Decode::Packet(packet.payload)));
        }
        if pcm_samples > DEADLINE {
            return None;
        }
        // A consecutive sequence with a timestamp jump (future DTX) is explicit
        // silence/discontinuity evidence. Do not apply FEC/PLC across that gap.
        if self.packets.iter().any(|p| {
            let distance = p.sequence.wrapping_sub(sequence) as i16;
            distance >= 0
                && p.timestamp.wrapping_sub(timestamp) as i32 != i32::from(distance) * FRAME as i32
        }) {
            self.metrics.discontinuities += 1;
            self.rebuffer(now);
            return None;
        }
        if self.missing >= 3 {
            self.rebuffer(now);
            return None;
        }
        let next = self.packets.iter().find(|p| {
            p.sequence == sequence.wrapping_add(1)
                && p.timestamp == timestamp.wrapping_add(FRAME as u32)
        });
        let decode = match next {
            Some(packet) => {
                self.metrics.fec_attempts += 1;
                Decode::Fec(packet.payload.clone())
            }
            None => Decode::Plc,
        };
        self.metrics.concealments += 1;
        self.missing += 1;
        self.advance();
        Some(self.decision(decode))
    }

    fn advance(&mut self) {
        if let Some((seq, ts)) = &mut self.expected {
            *seq = seq.wrapping_add(1);
            *ts = ts.wrapping_add(FRAME as u32);
        }
    }

    fn decision(&mut self, decode: Decode) -> Playout {
        Playout {
            decode,
            reset: std::mem::take(&mut self.reset),
            correction_ppm: self.metrics.correction_ppm,
        }
    }

    fn rebuffer(&mut self, now: Instant) {
        self.metrics.rebufferings += 1;
        // Worker stalls must not inflate measured network jitter. Drop stale
        // media only at this explicit discontinuity, never for normal drift.
        let previous = self.packets.len();
        self.packets
            .retain(|p| now.saturating_duration_since(p.arrived) <= Duration::from_millis(160));
        self.metrics.stale_discards += (previous - self.packets.len()) as u64;
        let sequence = self.expected.map(|p| p.0).unwrap_or(0);
        let first = self
            .packets
            .iter()
            .min_by_key(|p| p.sequence.wrapping_sub(sequence));
        if let Some(packet) = first {
            let (seq, ts, at) = (packet.sequence, packet.timestamp, packet.arrived);
            self.anchor(seq, ts, at.max(now));
        } else {
            self.playing = false;
            self.metrics.inactive = true;
        }
    }
}
