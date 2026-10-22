//! Portable receiver bitrate feedback; callers supply monotonic elapsed time.
use crate::screen::{INITIAL_VIEW_BITRATE, MAX_VIEW_BITRATE, MIN_VIEW_BITRATE};
use std::time::Duration;
/// One interval of receiver evidence, independent of stream kind or layer.
#[derive(Clone, Copy, Default)]
pub struct ReceiverSample {
    pub congested: bool,
    pub severe: bool,
    pub receiving: bool,
}

pub struct Receiver {
    maximum: u32,
    bitrate: u32,
    previous: Option<Duration>,
    started: Option<Duration>,
    changed: Option<Duration>,
    bad: u8,
    healthy: u8,
    idle: u8,
}
impl Default for Receiver {
    fn default() -> Self {
        Self::with_maximum(INITIAL_VIEW_BITRATE)
    }
}
impl Receiver {
    pub fn with_maximum(maximum: u32) -> Self {
        let maximum = maximum.clamp(MIN_VIEW_BITRATE, MAX_VIEW_BITRATE);
        Self {
            maximum,
            bitrate: maximum,
            previous: None,
            started: None,
            changed: None,
            bad: 0,
            healthy: 0,
            idle: 0,
        }
    }

    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Sample at most once per second. A stale interval clears evidence, retaining
    /// the last budget; callers recreate the controller for a new stream/viewer.
    pub fn sample_at(&mut self, sample: ReceiverSample, now: Duration) {
        if let Some(previous) = self.previous {
            let elapsed = now.saturating_sub(previous);
            if elapsed < Duration::from_secs(1) {
                return;
            }
            if elapsed > Duration::from_secs(3) {
                self.bad = 0;
                self.healthy = 0;
                self.idle = 0;
                self.started = Some(now);
                self.previous = Some(now);
                return;
            }
        }
        self.previous = Some(now);
        let started = *self.started.get_or_insert(now);
        self.idle = if sample.receiving {
            0
        } else {
            self.idle.saturating_add(1)
        };
        let severe = sample.severe || self.idle >= 3;
        let bad = sample.congested || severe;
        self.bad = ((self.bad << 1) | u8::from(bad)) & 0b1111;
        if bad {
            self.healthy = 0;
        } else if sample.receiving {
            self.healthy = self.healthy.saturating_add(1);
        } // Short idle gaps preserve evidence, but never prove spare capacity.
        if now.saturating_sub(started) < Duration::from_secs(2) {
            return; // Initial negotiation and IDR recovery.
        }
        let since_change = self.changed.map(|at| now.saturating_sub(at));
        let ready =
            |seconds| since_change.is_none_or(|elapsed| elapsed >= Duration::from_secs(seconds));
        if severe && ready(2) {
            self.decrease(65, now);
        } else if bad && self.bad.count_ones() >= 3 && ready(3) {
            self.decrease(85, now);
        } else if sample.receiving && self.healthy >= 6 && ready(3) {
            // Start probing after six receiving samples, then every three.
            // A low budget recovers in bounded time without a 25% step each time.
            let next = (self.bitrate * 112 / 100).min(self.maximum);
            if next != self.bitrate {
                self.bitrate = next;
                self.changed = Some(now);
            }
            self.healthy = 3;
        }
    }

    fn decrease(&mut self, percent: u32, now: Duration) {
        self.bitrate = (self.bitrate * percent / 100).max(MIN_VIEW_BITRATE);
        self.changed = Some(now);
        self.healthy = 0;
        self.bad = 0;
    }
}
