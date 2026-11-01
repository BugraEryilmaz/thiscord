//! Automatic microphone network adaptation. See docs/audio.md.
//!
//! Inputs are the authenticated server reception reports already used by the
//! adaptive FEC loss hint (loss and interarrival jitter for our microphone SSRC)
//! and the nominated ICE pair round-trip time. Delay growth and jitter indicate
//! congestion, so the bitrate steps down. Moderate loss without delay growth is
//! treated as random loss: the bitrate stays and DRED is requested instead.
//! Runs only on the native audio worker, once per accepted one-second window.
use super::adaptive_fec::Sample;
use std::time::{Duration, Instant};
use thiscord_shared::audio::{NetworkAdaptationDiagnostics, NetworkLevel};

/// Opus target per level (Good, Fair, Poor, Severe). 12 kbit/s is SILK
/// wideband, which receivers extend to fullband with OSCE.
const BITRATES: [i32; 4] = [32_000, 24_000, 16_000, 12_000];
const INTERVAL: Duration = Duration::from_secs(1);
const STALE: Duration = Duration::from_secs(6);
/// Consecutive worse windows required before stepping down.
const DEGRADE_WINDOWS: u8 = 2;
/// Sustained better windows required before each single step back up.
const RECOVER: Duration = Duration::from_secs(10);
/// Without usable feedback (silence/DTX or outage), keep the measured level
/// this long, then return to the default.
const FORGET: Duration = Duration::from_secs(60);
const LOSSY_WINDOWS: u8 = 2;
const DRED_CLEAR: Duration = Duration::from_secs(20);
/// Per accepted one-second window, raise the minimum RTT by 2% of the excess,
/// so a permanent route change does not look like congestion forever.
const RTT_BASELINE_RISE: f64 = 0.02;
/// Receivers: LACE plus deep PLC (5); OSCE wideband-to-fullband extension
/// is requested separately. NoLACE (7) measured about twice LACE's cost.
pub(crate) const DECODER_COMPLEXITY: i32 = 6;

#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) struct Window {
    pub loss_percent: f64,
    pub jitter_ms: f64,
    pub queueing_ms: Option<f64>,
}

pub(crate) fn classify(w: Window) -> NetworkLevel {
    let q = w.queueing_ms.unwrap_or(0.0);
    if w.loss_percent >= 20.0 || w.jitter_ms >= 80.0 || q >= 300.0 {
        NetworkLevel::Severe
    } else if w.loss_percent >= 12.0 || w.jitter_ms >= 50.0 || q >= 150.0 {
        NetworkLevel::Poor
    } else if w.jitter_ms >= 25.0 || q >= 60.0 {
        NetworkLevel::Fair
    } else {
        NetworkLevel::Good
    }
}

fn step_up(level: NetworkLevel) -> NetworkLevel {
    match level {
        NetworkLevel::Severe => NetworkLevel::Poor,
        NetworkLevel::Poor => NetworkLevel::Fair,
        _ => NetworkLevel::Good,
    }
}

pub(crate) fn bitrate(level: NetworkLevel) -> i32 {
    BITRATES[level as usize]
}

#[derive(Default)]
pub(crate) struct Controller {
    enabled: bool,
    source: Option<(u64, u64)>,
    baseline: Option<Sample>,
    seen: u64,
    rtt_min: Option<f64>,
    level: NetworkLevel,
    worse: u8,
    worse_level: NetworkLevel,
    calm_since: Option<Instant>,
    lossy: u8,
    dred: bool,
    clean_since: Option<Instant>,
    last_window: Option<Instant>,
    last: Option<Window>,
}
impl Controller {
    /// Returns (level, automatic DRED request). Disabled always returns the default.
    pub(crate) fn update(
        &mut self,
        enabled: bool,
        sample: Option<Sample>,
        rtt_ms: Option<f64>,
        now: Instant,
    ) -> (NetworkLevel, bool) {
        let source = sample.map(|s| (s.source, s.generation));
        if enabled != self.enabled || (source.is_some() && source != self.source) {
            // A new peer/source is a new path: start from the default.
            *self = Self {
                enabled,
                source,
                ..Default::default()
            };
        }
        if !enabled {
            return (NetworkLevel::Good, false);
        }
        if self
            .last_window
            .is_some_and(|at| now.saturating_duration_since(at) >= FORGET)
        {
            self.level = NetworkLevel::Good;
            self.dred = false;
            self.last_window = None;
            self.last = None;
            self.worse = 0;
            self.lossy = 0;
            self.calm_since = None;
            self.clean_since = None;
        }
        if let Some(window) = self.window(sample, rtt_ms, now) {
            self.apply(window, now);
        }
        (self.level, self.dred)
    }

    /// Same acceptance bounds as the adaptive FEC estimate: at least one second
    /// and 25 packets, under six seconds and 400 packets, monotonic counters.
    fn window(
        &mut self,
        sample: Option<Sample>,
        rtt_ms: Option<f64>,
        now: Instant,
    ) -> Option<Window> {
        let s = sample.filter(|s| now.saturating_duration_since(s.at) < STALE)?;
        if s.reports == self.seen {
            return None;
        }
        let reset = s.reports < self.seen;
        self.seen = s.reports;
        let Some(base) = self.baseline else {
            self.baseline = Some(s);
            return None;
        };
        let elapsed = s.at.saturating_duration_since(base.at);
        let expected = s.sequence.wrapping_sub(base.sequence);
        if reset || s.lost < base.lost || elapsed >= STALE || expected > 400 {
            self.baseline = Some(s);
            return None;
        }
        if elapsed < INTERVAL || expected < 25 {
            return None;
        }
        self.baseline = Some(s);
        let lost = s.lost - base.lost;
        if lost > expected {
            return None;
        }
        // Measure against the previous baseline, then age it once per
        // accepted window (not per encoded frame).
        let rtt = rtt_ms.filter(|r| r.is_finite() && *r >= 0.0);
        let queueing_ms = rtt.zip(self.rtt_min).map(|(rtt, min)| (rtt - min).max(0.0));
        if let Some(rtt) = rtt {
            self.rtt_min = Some(self.rtt_min.map_or(rtt, |min| {
                if rtt < min {
                    rtt
                } else {
                    min + (rtt - min) * RTT_BASELINE_RISE
                }
            }));
        }
        Some(Window {
            loss_percent: f64::from(lost) * 100.0 / f64::from(expected),
            jitter_ms: f64::from(s.jitter) / 48.0,
            queueing_ms,
        })
    }

    fn apply(&mut self, window: Window, now: Instant) {
        self.last_window = Some(now);
        self.last = Some(window);
        let measured = classify(window);
        if measured > self.level {
            // Fast but not instantaneous: two consecutive bad windows, then
            // jump to the milder of the two classifications.
            self.worse_level = if self.worse == 0 {
                measured
            } else {
                self.worse_level.min(measured)
            };
            self.worse += 1;
            self.calm_since = None;
            if self.worse >= DEGRADE_WINDOWS {
                self.level = self.worse_level;
                self.worse = 0;
            }
        } else {
            self.worse = 0;
            if measured < self.level {
                let since = *self.calm_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= RECOVER {
                    self.level = step_up(self.level);
                    self.calm_since = Some(now);
                }
            } else {
                self.calm_since = None;
            }
        }
        // DRED is a loss tool that costs bandwidth; never add it under congestion.
        if self.level != NetworkLevel::Good || measured != NetworkLevel::Good {
            self.dred = false;
            self.lossy = 0;
            self.clean_since = None;
        } else if window.loss_percent >= 5.0 {
            self.lossy = self.lossy.saturating_add(1);
            self.clean_since = None;
            if self.lossy >= LOSSY_WINDOWS {
                self.dred = true;
            }
        } else {
            self.lossy = 0;
            if window.loss_percent < 2.0 {
                let since = *self.clean_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= DRED_CLEAR {
                    self.dred = false;
                }
            } else {
                self.clean_since = None;
            }
        }
    }

    pub(crate) fn diagnostics(&self, now: Instant) -> NetworkAdaptationDiagnostics {
        NetworkAdaptationDiagnostics {
            enabled: self.enabled,
            level: self.level,
            automatic_dred: self.dred,
            uplink_loss_percent: self.last.map(|w| w.loss_percent),
            uplink_jitter_ms: self.last.map(|w| w.jitter_ms),
            queueing_delay_ms: self.last.and_then(|w| w.queueing_ms),
            feedback_age_ms: self.last_window.map(|at| {
                now.saturating_duration_since(at)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            }),
            ..Default::default()
        }
    }
}

/// Receive-side policy per remote microphone stream: use DRED recovery (and its
/// larger 80 ms buffer) only while that stream is losing packets and its sender
/// is actually emitting DRED. Evaluated once per second on the worker.
#[derive(Default)]
pub(crate) struct Receive {
    pub(super) wanted: bool,
    lossy: u8,
    clean_since: Option<Instant>,
    last: Option<(Instant, u64, u64)>,
    pub(super) padded_at: Option<Instant>,
}
const RECEIVE_PADDED: Duration = Duration::from_secs(10);
const RECEIVE_CLEAR: Duration = Duration::from_secs(30);
impl Receive {
    /// Opus code-3 packets with the padding flag: how DRED is carried.
    pub(super) fn observe_packet(&mut self, payload: &[u8], at: Instant) {
        if payload.len() > 2 && payload[0] & 0x03 == 0x03 && payload[1] & 0x40 != 0 {
            self.padded_at = Some(at);
        }
    }
    /// Desired automatic DRED state, from cumulative decoded packets and
    /// sequence gaps (packets never received or arriving after playout).
    pub(super) fn update(&mut self, now: Instant, played_total: u64, gaps_total: u64) -> bool {
        let Some((at, played, gaps)) = self.last else {
            self.last = Some((now, played_total, gaps_total));
            return self.wanted;
        };
        if now.saturating_duration_since(at) < INTERVAL {
            return self.wanted;
        }
        self.last = Some((now, played_total, gaps_total));
        let played = played_total.saturating_sub(played);
        let gaps = gaps_total.saturating_sub(gaps);
        let carrying = self
            .padded_at
            .is_some_and(|p| now.saturating_duration_since(p) < RECEIVE_PADDED);
        if played + gaps < 25 {
            // Silence or too little media to judge: hold, so each talkspurt
            // does not restart the receive buffer.
            return self.wanted;
        }
        let loss = gaps as f64 * 100.0 / (played + gaps) as f64;
        if loss >= 3.0 && carrying {
            self.clean_since = None;
            self.lossy = self.lossy.saturating_add(1);
            if self.lossy >= 2 {
                self.wanted = true;
            }
        } else {
            self.lossy = 0;
            if !carrying {
                // Active media without DRED for ten seconds: no donor benefit.
                self.wanted = false;
            } else if loss < 1.0 {
                let since = *self.clean_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= RECEIVE_CLEAR {
                    self.wanted = false;
                }
            } else {
                self.clean_since = None;
            }
        }
        self.wanted
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
