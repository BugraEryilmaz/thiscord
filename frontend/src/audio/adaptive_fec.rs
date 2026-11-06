//! Sender uplink feedback from authenticated RTCP reception reports only.
//! Pinned rtc 0.21 never calls its remote stats updater, so a narrow interceptor
//! observes reports before the default chain consumes them.
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::{audio::AdaptiveFecDiagnostics, voice::MediaKind};

const FALLBACK: u8 = 5;
const STALE: Duration = Duration::from_secs(6);
const INTERVAL: Duration = Duration::from_secs(1);

#[path = "adaptive_fec_rtcp.rs"]
mod rtcp;
pub use rtcp::register;

#[derive(Clone, Copy)]
pub(crate) struct Sample {
    pub(super) source: u64,
    pub(super) generation: u64,
    pub(super) reports: u64,
    pub(super) sequence: u32,
    pub(super) lost: u32,
    /// RFC 3550 interarrival jitter at the server, in 48 kHz RTP units.
    pub(super) jitter: u32,
    pub(super) at: Instant,
}

/// One fixed-size mailbox per peer. Only the owning authenticated DTLS-SRTP
/// transport writes it; the audio worker reads it, never a device callback.
/// The send task records every packet; within a source generation that is two
/// atomic operations, and only a generation change takes the lock.
pub struct Feedback {
    state: Mutex<Mailbox>,
    /// The mailbox's generation, readable without the lock (`NONE` before any).
    generation: AtomicU64,
    /// Extended highest sent sequence; bit 32 marks a value.
    sent: AtomicU64,
}
impl Default for Feedback {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            generation: AtomicU64::new(NONE),
            sent: AtomicU64::new(0),
        }
    }
}
const NONE: u64 = u64::MAX;
fn unpack(packed: u64) -> Option<u32> {
    (packed >> 32 == 1).then_some(packed as u32)
}
fn pack(extended: u32) -> u64 {
    1 << 32 | u64::from(extended)
}
fn extend(old: Option<u32>, sequence: u16) -> u32 {
    old.map_or(u32::from(sequence), |old| {
        old.wrapping_add(u32::from(sequence.wrapping_sub(old as u16)))
    })
}
#[derive(Default)]
struct Mailbox {
    sample: Option<Sample>,
    generation: Option<u64>,
    first: u32,
}
impl Feedback {
    /// Single writer: the peer's send task.
    pub(super) fn sent(&self, generation: u64, sequence: u16) {
        if self.generation.load(Ordering::Acquire) == generation {
            let extended = extend(unpack(self.sent.load(Ordering::Acquire)), sequence);
            self.sent.store(pack(extended), Ordering::Release);
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            if state.generation.is_some_and(|old| old > generation) {
                return;
            }
            let extended = extend(unpack(self.sent.load(Ordering::Acquire)), sequence);
            if state.generation != Some(generation) {
                state.generation = Some(generation);
                state.first = extended;
                state.sample = None;
            }
            self.sent.store(pack(extended), Ordering::Release);
            self.generation.store(generation, Ordering::Release);
        }
    }
    pub(super) fn observe(
        &self,
        source: u64,
        report: &rtc::rtcp::reception_report::ReceptionReport,
        now: Instant,
    ) {
        if report.ssrc != MediaKind::Microphone.publisher_ssrc() || report.total_lost >= 0x7fffff {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let Some(generation) = state.generation else {
                return;
            };
            let Some(sent) = unpack(self.sent.load(Ordering::Acquire)) else {
                return;
            };
            // Report must acknowledge packets sent in this source generation,
            // never an old source or a sequence beyond actual transmission.
            if report.last_sequence_number.wrapping_sub(state.first)
                > sent.wrapping_sub(state.first)
            {
                return;
            }
            if state.sample.is_some_and(|old| {
                old.source == source
                    && (report.last_sequence_number.wrapping_sub(old.sequence) > 0x7fff_ffff
                        || (old.sequence == report.last_sequence_number
                            && old.lost == report.total_lost)
                        || now.saturating_duration_since(old.at) < INTERVAL)
            }) {
                return;
            }
            let reports = state
                .sample
                .map_or(1, |old| old.reports.wrapping_add(1).max(1));
            state.sample = Some(Sample {
                source,
                generation,
                reports,
                sequence: report.last_sequence_number,
                lost: report.total_lost,
                jitter: report.jitter,
                at: now,
            });
        }
    }
    pub(crate) fn sample(&self, generation: u64) -> Option<Sample> {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.sample.filter(|sample| sample.generation == generation))
    }
}

pub(crate) struct Controller {
    enabled: bool,
    source: Option<(u64, u64)>,
    floor: u8,
    baseline: Option<Sample>,
    seen: u64,
    valid_at: Option<Instant>,
    smoothed: f64,
    hint: u8,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            enabled: false,
            source: None,
            floor: FALLBACK,
            baseline: None,
            seen: 0,
            valid_at: None,
            smoothed: f64::from(FALLBACK),
            hint: FALLBACK,
        }
    }
}
impl Controller {
    pub(crate) fn update(
        &mut self,
        enabled: bool,
        floor: u8,
        sample: Option<Sample>,
        now: Instant,
    ) -> u8 {
        let floor = floor.clamp(FALLBACK, 30);
        let source = sample.map(|s| (s.source, s.generation));
        if enabled != self.enabled || source != self.source || floor != self.floor {
            *self = Self {
                enabled,
                source,
                floor,
                hint: floor,
                smoothed: f64::from(floor),
                ..Default::default()
            };
        }
        if !enabled {
            return self.floor;
        }
        if self
            .valid_at
            .is_some_and(|at| now.saturating_duration_since(at) >= STALE)
        {
            self.hint = self.floor;
            self.smoothed = f64::from(self.floor);
            self.valid_at = None;
            self.baseline = None;
        }
        let Some(s) = sample.filter(|s| now.saturating_duration_since(s.at) < STALE) else {
            return self.hint;
        };
        if s.reports == self.seen {
            return self.hint;
        }
        let reset = s.reports < self.seen;
        self.seen = s.reports;
        let Some(base) = self.baseline else {
            self.baseline = Some(s);
            return self.hint;
        };
        let elapsed = s.at.saturating_duration_since(base.at);
        let expected = s.sequence.wrapping_sub(base.sequence);
        // Bound samples to six seconds / 400 packets. Counter resets, impossible
        // jumps and delayed reports cannot become a huge loss estimate. Extended
        // 16-bit RTP sequence wrap (and eventual 32-bit wrap) remains continuous.
        if reset || s.lost < base.lost || elapsed >= STALE || expected > 400 {
            self.baseline = Some(s);
            self.valid_at = None;
            self.smoothed = f64::from(self.floor);
            self.hint = self.floor;
            return self.hint;
        }
        if elapsed < INTERVAL || expected < 25 {
            return self.hint;
        }
        self.baseline = Some(s);
        let lost = s.lost - base.lost;
        if lost > expected {
            return self.hint;
        }
        let measured =
            (f64::from(lost) * 100.0 / f64::from(expected)).clamp(f64::from(self.floor), 30.0);
        self.smoothed += 0.25 * (measured - self.smoothed);
        let target = self.smoothed.round() as u8;
        // One percentage point of hysteresis, at most +3/-1 points per window.
        if target.abs_diff(self.hint) >= 2 {
            self.hint = target.clamp(self.hint.saturating_sub(1), (self.hint + 3).min(30));
        }
        self.valid_at = Some(s.at);
        self.hint
    }
    pub(crate) fn diagnostics(&self, now: Instant) -> AdaptiveFecDiagnostics {
        AdaptiveFecDiagnostics {
            enabled: self.enabled,
            baseline_loss_percent: self.floor,
            effective_loss_percent: self.hint,
            feedback_age_ms: self.valid_at.map(|at| {
                now.saturating_duration_since(at)
                    .as_millis()
                    .min(u64::MAX as u128) as u64
            }),
            adapting: self.enabled
                && self
                    .valid_at
                    .is_some_and(|at| now.saturating_duration_since(at) < STALE),
        }
    }
}

#[cfg(test)]
#[path = "adaptive_fec_tests.rs"]
mod tests;
