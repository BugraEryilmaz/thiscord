//! Receiver AIMD budget and expiring publisher demand. No media crosses IPC.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use thiscord_shared::screen::{
    INITIAL_VIEW_BITRATE, MAX_VIEW_BITRATE, MIN_VIEW_BITRATE, VIEW_LEASE_SECS,
};

#[derive(Default)]
pub struct Demand(Mutex<Option<(u32, Instant)>>);
impl Demand {
    pub fn update(&self, bitrate: u32) {
        let bitrate = if (MIN_VIEW_BITRATE..=MAX_VIEW_BITRATE).contains(&bitrate) {
            bitrate
        } else {
            0
        };
        *self.0.lock().unwrap() = Some((bitrate, Instant::now()));
    }
    pub fn bitrate(&self) -> u32 {
        self.0
            .lock()
            .unwrap()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(VIEW_LEASE_SECS))
            .map_or(0, |(bitrate, _)| bitrate)
    }
}

pub struct Receiver {
    bitrate: u32,
    healthy: u8,
    samples: u8,
    idle: u8,
}
impl Default for Receiver {
    fn default() -> Self {
        Self {
            bitrate: INITIAL_VIEW_BITRATE,
            healthy: 0,
            samples: 0,
            idle: 0,
        }
    }
}
impl Receiver {
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }
    /// One sample per second; fast decrease, ten healthy seconds before probing up.
    pub fn sample(&mut self, congested: bool, receiving: bool) {
        self.samples = self.samples.saturating_add(1);
        if self.samples < 3 {
            return;
        } // Allow initial negotiation/IDR recovery.
        self.idle = if receiving {
            0
        } else {
            self.idle.saturating_add(1)
        };
        if !congested && !receiving && self.idle < 3 {
            return;
        }
        if congested || self.idle >= 3 {
            self.bitrate = (self.bitrate * 65 / 100).max(MIN_VIEW_BITRATE);
            self.healthy = 0;
        } else {
            self.healthy += 1;
            if self.healthy >= 10 {
                self.bitrate = (self.bitrate * 125 / 100).min(MAX_VIEW_BITRATE);
                self.healthy = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn congestion_reduces_budget_and_recovery_requires_sustained_health() {
        let mut receiver = Receiver::default();
        for _ in 0..3 {
            receiver.sample(true, true);
        }
        assert_eq!(receiver.bitrate(), INITIAL_VIEW_BITRATE * 65 / 100);
        let reduced = receiver.bitrate();
        for _ in 0..9 {
            receiver.sample(false, true);
        }
        assert_eq!(receiver.bitrate(), reduced);
        receiver.sample(false, true);
        assert!(receiver.bitrate() > reduced);
        for _ in 0..100 {
            receiver.sample(false, false);
        }
        assert_eq!(receiver.bitrate(), MIN_VIEW_BITRATE);
        for _ in 0..1000 {
            receiver.sample(false, true);
        }
        assert_eq!(receiver.bitrate(), MAX_VIEW_BITRATE);
    }
    #[test]
    fn publisher_pauses_without_fresh_demand() {
        let demand = Demand::default();
        assert_eq!(demand.bitrate(), 0);
        demand.update(INITIAL_VIEW_BITRATE);
        assert_eq!(demand.bitrate(), INITIAL_VIEW_BITRATE);
        demand.0.lock().unwrap().as_mut().unwrap().1 -= Duration::from_secs(VIEW_LEASE_SECS);
        assert_eq!(demand.bitrate(), 0);
        demand.update(0);
        assert_eq!(demand.bitrate(), 0);
        demand.update(u32::MAX);
        assert_eq!(demand.bitrate(), 0);
    }
    #[test]
    fn static_source_gaps_do_not_force_quality_to_the_floor() {
        let mut receiver = Receiver::default();
        for _ in 0..30 {
            receiver.sample(false, true);
            receiver.sample(false, false);
        }
        assert!(receiver.bitrate() >= INITIAL_VIEW_BITRATE);
        let previous = receiver.bitrate();
        for _ in 0..3 {
            receiver.sample(false, false);
        }
        assert!(receiver.bitrate() < previous);
    }
}
