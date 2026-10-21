//! Per-viewer feedback and per-encoder quality selection. No media crosses IPC.
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{
    INITIAL_VIEW_BITRATE, MAX_VIEW_BITRATE, MIN_VIEW_BITRATE, VIEW_LEASE_SECS,
};

#[derive(Default)]
pub struct Demand(Mutex<Option<(u32, Instant)>>, AtomicU32);
impl Demand {
    pub fn update(&self, bitrate: u32) {
        let bitrate = if (MIN_VIEW_BITRATE..=MAX_VIEW_BITRATE).contains(&bitrate) {
            bitrate
        } else {
            0
        };
        *self.0.lock().unwrap() = Some((bitrate, Instant::now()));
    }
    /// A measured encoded burst cannot fit the receiver assembly deadline.
    /// Keep this signal local to the affected encoder; it is not viewer feedback.
    pub fn limit_encoder(&self, bitrate: u32) {
        let bitrate = bitrate.max(MIN_VIEW_BITRATE);
        let _ = self
            .1
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(if old == 0 { bitrate } else { old.min(bitrate) })
            });
    }
    pub fn take_encoder_limit(&self) -> u32 {
        self.1.swap(0, Ordering::AcqRel)
    }
    pub fn bitrate(&self) -> u32 {
        self.0
            .lock()
            .unwrap()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(VIEW_LEASE_SECS))
            .map_or(0, |(bitrate, _)| bitrate)
    }
}

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
    previous: Option<Instant>,
    started: Option<Instant>,
    changed: Option<Instant>,
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
    pub fn sample_at(&mut self, sample: ReceiverSample, now: Instant) {
        if let Some(previous) = self.previous {
            let elapsed = now.saturating_duration_since(previous);
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
        if now.saturating_duration_since(started) < Duration::from_secs(2) {
            return; // Initial negotiation and IDR recovery.
        }
        let since_change = self.changed.map(|at| now.saturating_duration_since(at));
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

    fn decrease(&mut self, percent: u32, now: Instant) {
        self.bitrate = (self.bitrate * percent / 100).max(MIN_VIEW_BITRATE);
        self.changed = Some(now);
        self.healthy = 0;
        self.bad = 0;
    }
}

/// Stabilizes discrete resolution/FPS transitions that recreate an encoder.
/// Keep one controller per independently encoded stream/layer. Zero demand resets
/// all pending transitions immediately and never delays the publisher pause.
pub struct QualityController {
    maximum: thiscord_shared::screen::Quality,
    current: Option<thiscord_shared::screen::Quality>,
    pending: Option<(thiscord_shared::screen::Quality, Instant)>,
    changed: Option<Instant>,
}
impl QualityController {
    pub fn new(maximum: thiscord_shared::screen::Quality) -> Self {
        Self {
            maximum,
            current: None,
            pending: None,
            changed: None,
        }
    }

    /// An actual oversized access unit bypasses dwell/cooldown only to lower
    /// quality. Normal upgrades still require stable headroom after recovery.
    pub fn update_with_recovery(
        &mut self,
        bitrate: u32,
        encoder_limit: u32,
        now: Instant,
    ) -> Option<thiscord_shared::screen::Quality> {
        if bitrate > 0 && encoder_limit > 0 {
            let next = self
                .maximum
                .constrained(bitrate.min(encoder_limit).max(MIN_VIEW_BITRATE));
            if self
                .current
                .is_some_and(|current| next.bitrate() < current.bitrate())
            {
                return Some(self.select(next, now));
            }
        }
        self.update(bitrate, now)
    }

    pub fn update(
        &mut self,
        bitrate: u32,
        now: Instant,
    ) -> Option<thiscord_shared::screen::Quality> {
        if bitrate == 0 {
            self.current = None;
            self.pending = None;
            self.changed = None;
            return None;
        }
        let bitrate = bitrate.clamp(MIN_VIEW_BITRATE, MAX_VIEW_BITRATE);
        let candidate = self.maximum.constrained(bitrate);
        let Some(current) = self.current else {
            return Some(self.select(candidate, now));
        };
        if candidate == current {
            self.pending = None;
            return Some(current);
        }
        let score =
            |q: thiscord_shared::screen::Quality| u64::from(q.height).pow(2) * u64::from(q.fps);
        let upgrading = score(candidate) > score(current);
        if !upgrading && bitrate < current.bitrate() * 60 / 100 {
            // A large capacity collapse must not wait for the rebuild cooldown.
            return Some(self.select(candidate, now));
        }
        let next = if upgrading {
            // Twenty percent headroom prevents repeated up/down transitions near
            // a ladder boundary. The selected maximum remains reachable at its
            // nominal budget (including the 32 Mbit/s wire ceiling).
            let headroom = self.maximum.constrained(bitrate * 100 / 120);
            if bitrate >= self.maximum.bitrate() {
                self.maximum
            } else {
                headroom
            }
        } else {
            candidate
        };
        if score(next) <= score(current) && upgrading {
            self.pending = None;
            return Some(current);
        }
        let since = match self.pending {
            Some((pending, since))
                if pending == next || (!upgrading && score(pending) < score(current)) =>
            {
                since
            }
            _ => {
                self.pending = Some((next, now));
                now
            }
        };
        let dwell = Duration::from_secs(if upgrading { 6 } else { 2 });
        if now.saturating_duration_since(since) >= dwell
            && self
                .changed
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_secs(5))
        {
            return Some(self.select(next, now));
        }
        Some(current)
    }

    fn select(
        &mut self,
        quality: thiscord_shared::screen::Quality,
        now: Instant,
    ) -> thiscord_shared::screen::Quality {
        self.current = Some(quality);
        self.pending = None;
        self.changed = Some(now);
        quality
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::screen::Quality;

    struct Link {
        receiver: Receiver,
        start: Instant,
        second: u64,
    }
    impl Link {
        fn new(maximum: u32) -> Self {
            Self {
                receiver: Receiver::with_maximum(maximum),
                start: Instant::now(),
                second: 0,
            }
        }
        fn step(&mut self, congested: bool, severe: bool, receiving: bool) -> u32 {
            self.receiver.sample_at(
                ReceiverSample {
                    congested,
                    severe,
                    receiving,
                },
                self.start + Duration::from_secs(self.second),
            );
            self.second += 1;
            self.receiver.bitrate()
        }
        fn run(&mut self, seconds: u64, congested: bool, severe: bool, receiving: bool) -> u32 {
            for _ in 0..seconds {
                self.step(congested, severe, receiving);
            }
            self.receiver.bitrate()
        }
    }

    #[test]
    fn isolated_spikes_and_alternating_threshold_noise_do_not_cut_budget() {
        let mut link = Link::new(MAX_VIEW_BITRATE);
        link.run(10, false, false, true);
        for _ in 0..20 {
            assert_eq!(link.step(true, false, true), MAX_VIEW_BITRATE);
            assert_eq!(link.step(false, false, true), MAX_VIEW_BITRATE);
        }
        link.run(3, false, false, true);
        link.run(2, true, false, true);
        assert_eq!(link.run(10, false, false, true), MAX_VIEW_BITRATE);
    }

    #[test]
    fn sustained_congestion_steps_down_without_a_per_second_collapse() {
        let mut link = Link::new(5_000_000);
        link.run(5, false, false, true);
        assert_eq!(link.run(2, true, false, true), 5_000_000);
        assert_eq!(link.step(true, false, true), 4_250_000);
        assert_eq!(link.run(2, true, false, true), 4_250_000);
        assert_eq!(link.step(true, false, true), 3_612_500);
        assert_eq!(link.run(300, true, false, true), MIN_VIEW_BITRATE);
    }

    #[test]
    fn severe_loss_and_missing_frames_have_a_faster_bounded_response() {
        let mut link = Link::new(5_000_000);
        assert_eq!(link.run(2, true, true, true), 5_000_000); // Setup grace.
        assert_eq!(link.step(true, true, true), 3_250_000);
        assert_eq!(link.step(true, true, true), 3_250_000);
        assert_eq!(link.step(true, true, true), 2_112_500);
        let mut stalled = Link::new(5_000_000);
        stalled.run(5, false, false, true);
        assert_eq!(stalled.run(2, false, false, false), 5_000_000);
        assert_eq!(stalled.step(false, false, false), 3_250_000);
        assert_eq!(stalled.run(60, false, false, false), MIN_VIEW_BITRATE);
    }

    #[test]
    fn healthy_ceiling_does_not_delay_a_new_severe_event() {
        let mut link = Link::new(5_000_000);
        link.run(6, false, false, true);
        assert_eq!(link.step(true, true, true), 3_250_000);
    }

    #[test]
    fn layered_targets_preserve_independent_hysteresis_pacing_and_static_recovery() {
        use crate::screen::{keyframe::KeyframePolicy, pacing::*};
        use thiscord_shared::screen::LayerTargets;
        let maximum = Quality::default();
        let low_maximum = LayerTargets::low_quality(maximum);
        let mut controllers = [
            QualityController::new(maximum),
            QualityController::new(low_maximum),
        ];
        let mut budgets = [SendBudget::new(1), SendBudget::new(1)];
        let mut keys = [KeyframePolicy::default(), KeyframePolicy::default()];
        let start = Instant::now();
        let plan = LayerTargets::plan(maximum, [5_000_000, 1_250_000].into_iter());
        for (i, demand) in [plan.primary, plan.low].into_iter().enumerate() {
            let quality = controllers[i].update(demand, start).unwrap();
            budgets[i].set_bitrate(transport_budget(quality.bitrate(), demand));
            keys[i].submitted(start, true);
        }
        // Frames already queued must follow their own encoder's shared live rate.
        let queued = [budgets[0].track(1000), budgets[1].track(1000)];
        let plan = LayerTargets::plan(maximum, [4_750_000, 1_100_000].into_iter());
        assert_eq!((plan.primary, plan.low), (4_750_000, 1_100_000));
        for (i, demand) in [plan.primary, plan.low].into_iter().enumerate() {
            let now = start + Duration::from_secs(1);
            let held = controllers[i].update(demand, now).unwrap();
            assert_eq!(held, [maximum, low_maximum][i]);
            budgets[i].set_bitrate(transport_budget(held.bitrate(), demand));
            assert_eq!(queued[i].pacing_bitrate(demand), transport_bitrate(demand));
            assert!(keys[i].refresh_due(now, false));
            assert!(!keys[i].periodic_due(now));
            keys[i].submitted(now, false);
            let reduced = controllers[i]
                .update(demand, start + Duration::from_secs(5))
                .unwrap();
            assert!(reduced.bitrate() <= demand);
        }
        // Low-layer feedback never consumes the primary's recovery timer.
        let now = start + Duration::from_millis(1200);
        assert!(keys[1].feedback_ready(now));
        keys[1].submitted(now, true);
        assert!(keys[0].periodic_due(start + Duration::from_secs(10)));
        assert!(!keys[1].periodic_due(start + Duration::from_secs(10)));
        for (i, demand) in [5_000_000, 1_250_000].into_iter().enumerate() {
            controllers[i].update(demand, start + Duration::from_secs(6));
            assert_eq!(
                controllers[i].update(demand, start + Duration::from_secs(12)),
                Some([maximum, low_maximum][i])
            );
        }
        assert_eq!(
            controllers[1].update(0, start + Duration::from_secs(13)),
            None
        );
        assert_eq!(
            controllers[0].update(5_000_000, start + Duration::from_secs(13)),
            Some(maximum)
        );

        // A returning weak static viewer recovers through the low layer and
        // eventually switches to primary, without changing the fast viewer.
        let mut slow = Link::new(5_000_000);
        slow.run(60, true, true, true);
        let mut selected = 1;
        let mut switches = 0;
        for second in 0..320 {
            let receiving = second % 2 == 0;
            let demand = slow.step(false, false, receiving);
            let plan = LayerTargets::plan(maximum, [5_000_000, demand].into_iter());
            let now = start + Duration::from_secs(20 + second);
            assert_eq!(controllers[0].update(plan.primary, now), Some(maximum));
            let low = controllers[1].update(plan.low, now);
            assert_eq!(low.is_some(), plan.low > 0);
            let next = plan.layer(demand);
            switches += usize::from(next != selected);
            selected = next;
        }
        assert_eq!(slow.receiver.bitrate(), 5_000_000);
        assert_eq!((selected, switches), (0, 1));
    }

    #[test]
    fn combined_budget_and_encoder_trajectory_recovers_without_rebuild_chatter() {
        let maximum = Quality {
            height: 2160,
            fps: 60,
        };
        let mut link = Link::new(MAX_VIEW_BITRATE);
        let mut encoder = QualityController::new(maximum);
        let mut changes = Vec::new();
        let mut previous = None;
        for second in 0..200 {
            let budget = match second {
                10 | 11 => link.step(true, false, true), // A two-second burst.
                20..=31 => link.step(true, false, true),
                32..=43 => link.step(true, true, true),
                _ => link.step(false, false, true),
            };
            for tick in 0..10 {
                let current = encoder.update(
                    budget,
                    link.start + Duration::from_millis(second * 1000 + tick * 100),
                );
                if current != previous {
                    changes.push((second, current));
                    previous = current;
                }
            }
            if second < 20 {
                assert_eq!(previous, Some(maximum));
            }
        }
        assert_eq!(previous, Some(maximum));
        assert_eq!(link.receiver.bitrate(), MAX_VIEW_BITRATE);
        assert!(changes.len() > 5 && changes.len() < 25, "{changes:?}");
        // Once recovery starts, all actual encoder upgrades respect cooldown.
        let recovery: Vec<_> = changes.iter().filter(|(second, _)| *second > 44).collect();
        assert!(recovery.windows(2).all(|pair| pair[1].0 - pair[0].0 >= 5));
    }

    #[test]
    fn recovery_waits_for_health_then_reaches_screen_and_camera_maxima() {
        for maximum in [MAX_VIEW_BITRATE, thiscord_shared::camera::MAX_BITRATE] {
            let mut link = Link::new(maximum);
            link.run(60, true, true, true);
            assert_eq!(link.receiver.bitrate(), MIN_VIEW_BITRATE);
            assert_eq!(link.run(5, false, false, true), MIN_VIEW_BITRATE);
            assert_eq!(link.step(false, false, true), 224_000);
            assert_eq!(link.run(2, false, false, true), 224_000);
            assert_eq!(link.step(false, false, true), 250_880);
            // 32 Mbit/s recovers from the floor within 150 healthy seconds.
            assert_eq!(link.run(140, false, false, true), maximum);
            assert_eq!(link.run(100, false, false, true), maximum);
        }
    }

    #[test]
    fn static_cadence_and_duplicate_polls_are_not_congestion() {
        let mut link = Link::new(MAX_VIEW_BITRATE);
        for _ in 0..60 {
            link.step(false, false, true);
            assert_eq!(link.step(false, false, false), MAX_VIEW_BITRATE);
        }
        let at = link.start + Duration::from_secs(link.second);
        for _ in 0..100 {
            link.receiver.sample_at(
                ReceiverSample {
                    congested: true,
                    ..Default::default()
                },
                at,
            );
        }
        assert_eq!(link.receiver.bitrate(), MAX_VIEW_BITRATE);
    }

    #[test]
    fn sparse_receiving_samples_recover_below_the_ceiling_without_idle_probes() {
        for maximum in [MAX_VIEW_BITRATE, thiscord_shared::camera::MAX_BITRATE] {
            let mut link = Link::new(maximum);
            link.run(5, false, false, true);
            let cut = link.step(true, true, true);
            assert!(cut < maximum);
            for sample in 1..=60 {
                let before = link.receiver.bitrate();
                assert_eq!(link.step(false, false, false), before);
                let after = link.step(false, false, true);
                if sample < 6 {
                    assert_eq!(after, cut);
                }
            }
            assert_eq!(link.receiver.bitrate(), maximum);
            link.run(60, true, true, true);
            assert_eq!(link.receiver.bitrate(), MIN_VIEW_BITRATE);
            // Recovery also completes from the floor, at half the sampling duty.
            for _ in 0..150 {
                let before = link.receiver.bitrate();
                assert_eq!(link.step(false, false, false), before);
                link.step(false, false, true);
            }
            assert_eq!(link.receiver.bitrate(), maximum);
        }
    }

    #[test]
    fn static_delta_heartbeats_recover_but_missing_submissions_still_cut_budget() {
        use crate::screen::keyframe::KeyframePolicy;
        let mut link = Link::new(MAX_VIEW_BITRATE);
        link.run(5, false, false, true);
        assert!(link.step(true, true, true) < MAX_VIEW_BITRATE);
        let mut policy = KeyframePolicy::default();
        let mut idrs = 0;
        let mut deltas = 0;
        for _ in 0..40 {
            let now = link.start + Duration::from_secs(link.second);
            assert!(policy.refresh_due(now, false));
            let forced = policy.periodic_due(now);
            policy.submitted(now, forced);
            idrs += u32::from(forced);
            deltas += u32::from(!forced);
            link.step(false, false, true);
        }
        assert_eq!((idrs, deltas), (4, 36));
        assert_eq!(link.receiver.bitrate(), MAX_VIEW_BITRATE);
        // Busy/stopped capture does not submit the due heartbeat; no frame
        // evidence is fabricated merely because refresh_due remains true.
        for _ in 0..2 {
            let now = link.start + Duration::from_secs(link.second);
            assert!(policy.refresh_due(now, false));
            assert_eq!(link.step(false, false, false), MAX_VIEW_BITRATE);
        }
        assert_eq!(link.step(false, false, false), MAX_VIEW_BITRATE * 65 / 100);
    }

    #[test]
    fn stale_sampling_does_not_count_suspended_time_as_loss_or_recovery() {
        let mut link = Link::new(5_000_000);
        link.run(5, false, false, true);
        link.run(2, true, false, true);
        link.second += 20;
        assert_eq!(link.step(true, true, false), 5_000_000);
        assert_eq!(link.run(2, false, false, true), 5_000_000);
        link.run(3, true, false, true);
        let reduced = link.receiver.bitrate();
        link.run(5, false, false, true);
        link.second += 20;
        assert_eq!(link.run(5, false, false, true), reduced);
    }

    #[test]
    fn controllers_are_independent_and_new_viewers_start_fresh() {
        let mut slow = Link::new(MAX_VIEW_BITRATE);
        let mut fast = Link::new(MAX_VIEW_BITRATE);
        slow.run(90, true, true, true);
        assert_eq!(slow.receiver.bitrate(), MIN_VIEW_BITRATE);
        assert_eq!(fast.run(90, false, false, true), MAX_VIEW_BITRATE);
        assert_eq!(Receiver::default().bitrate(), MAX_VIEW_BITRATE);
        assert_eq!(Receiver::with_maximum(0).bitrate(), MIN_VIEW_BITRATE);
        assert_eq!(Receiver::with_maximum(u32::MAX).bitrate(), MAX_VIEW_BITRATE);
    }

    #[test]
    fn publisher_pauses_on_expiry_and_resumes_without_stale_transition_state() {
        let demand = Demand::default();
        let mut quality = QualityController::new(Quality::default());
        let now = Instant::now();
        assert_eq!(quality.update(demand.bitrate(), now), None);
        demand.update(INITIAL_VIEW_BITRATE);
        assert_eq!(
            quality.update(demand.bitrate(), now),
            Some(Quality::default())
        );
        quality.update(4_000_000, now + Duration::from_secs(1));
        demand.0.lock().unwrap().as_mut().unwrap().1 -= Duration::from_secs(VIEW_LEASE_SECS);
        assert_eq!(
            quality.update(demand.bitrate(), now + Duration::from_secs(2)),
            None
        );
        demand.update(4_000_000);
        assert_eq!(
            quality.update(demand.bitrate(), now + Duration::from_secs(3)),
            Some(Quality::default().constrained(4_000_000))
        );
        for invalid in [0, 1, u32::MAX] {
            demand.update(invalid);
            assert_eq!(demand.bitrate(), 0);
        }
    }

    #[test]
    fn quality_ignores_brief_budget_dips_and_boundary_oscillation() {
        let maximum = Quality::default();
        let mut controller = QualityController::new(maximum);
        let start = Instant::now();
        assert_eq!(controller.update(5_000_000, start), Some(maximum));
        for second in 1..60 {
            let budget = if second % 3 == 0 {
                5_100_000
            } else {
                4_900_000
            };
            assert_eq!(
                controller.update(budget, start + Duration::from_secs(second)),
                Some(maximum)
            );
        }
        let reduced = maximum.constrained(4_900_000);
        assert_eq!(
            controller.update(4_900_000, start + Duration::from_secs(60)),
            Some(reduced)
        );
    }

    #[test]
    fn quality_rebuilds_require_dwell_cooldown_and_recovery_headroom() {
        let maximum = Quality {
            height: 2160,
            fps: 60,
        };
        let mut controller = QualityController::new(maximum);
        let start = Instant::now();
        let low = maximum.constrained(4_000_000);
        assert_eq!(controller.update(4_000_000, start), Some(low));
        // A budget just above the next threshold cannot oscillate the encoder.
        for second in 1..20 {
            assert_eq!(
                controller.update(5_100_000, start + Duration::from_secs(second)),
                Some(low)
            );
        }
        for second in 20..26 {
            assert_eq!(
                controller.update(6_000_000, start + Duration::from_secs(second)),
                Some(low)
            );
        }
        let upgraded = controller
            .update(6_000_000, start + Duration::from_secs(26))
            .unwrap();
        assert!(upgraded.bitrate() > low.bitrate());
        for second in 27..31 {
            assert_eq!(
                controller.update(4_000_000, start + Duration::from_secs(second)),
                Some(upgraded)
            );
        }
        assert_eq!(
            controller.update(4_000_000, start + Duration::from_secs(31)),
            Some(low)
        );
        for second in 32..38 {
            assert_eq!(
                controller.update(MAX_VIEW_BITRATE, start + Duration::from_secs(second)),
                Some(low)
            );
        }
        assert_eq!(
            controller.update(MAX_VIEW_BITRATE, start + Duration::from_secs(38)),
            Some(maximum)
        );
        // Severe collapse bypasses cooldown even immediately after an upgrade.
        assert_eq!(
            controller.update(MIN_VIEW_BITRATE, start + Duration::from_secs(39)),
            Some(maximum.constrained(MIN_VIEW_BITRATE))
        );
    }

    #[test]
    fn moving_downward_targets_do_not_postpone_a_sustained_downgrade() {
        let maximum = Quality::default();
        let start = Instant::now();
        let mut controller = QualityController::new(maximum);
        controller.update(maximum.bitrate(), start);
        controller.update(4_600_000, start + Duration::from_secs(10));
        controller.update(3_600_000, start + Duration::from_secs(11));
        assert_eq!(
            controller.update(4_000_000, start + Duration::from_secs(12)),
            Some(maximum.constrained(4_000_000))
        );
    }
}
