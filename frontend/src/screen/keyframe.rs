//! Recovery and static-source refresh are separate clocks. A heartbeat does not
//! need to discard the decoder's reference picture every second.
use std::time::{Duration, Instant};

pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(10);
pub const FEEDBACK_INTERVAL: Duration = Duration::from_millis(200);
pub const STATIC_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub fn gop_frames(fps: u32) -> u32 {
    fps.saturating_mul(RECOVERY_INTERVAL.as_secs() as u32)
}

#[derive(Default)]
pub struct KeyframePolicy {
    last_keyframe: Option<Instant>,
    last_input: Option<Instant>,
}

impl KeyframePolicy {
    pub fn feedback_ready(&self, now: Instant) -> bool {
        self.last_keyframe
            .is_none_or(|at| now.saturating_duration_since(at) >= FEEDBACK_INTERVAL)
    }

    pub fn periodic_due(&self, now: Instant) -> bool {
        self.last_keyframe
            .is_none_or(|at| now.saturating_duration_since(at) >= RECOVERY_INTERVAL)
    }

    /// Called only without fresh capture. Preserve heartbeat traffic for stall
    /// detection, and allow feedback to refresh a static source immediately.
    pub fn refresh_due(&self, now: Instant, feedback: bool) -> bool {
        self.periodic_due(now)
            || (feedback && self.feedback_ready(now))
            || self
                .last_input
                .is_none_or(|at| now.saturating_duration_since(at) >= STATIC_REFRESH_INTERVAL)
    }

    /// Commit only after hardware input submission. Busy hardware/backpressure
    /// must neither advance clocks nor consume the caller's feedback flag.
    pub fn submitted(&mut self, now: Instant, forced: bool) {
        self.last_input = Some(now);
        if forced {
            self.observed(now);
        }
    }

    /// Automatic/scene-change IDRs also reset recovery spacing. Starting the
    /// clock at request time above avoids repeated requests while output is pending.
    pub fn observed(&mut self, now: Instant) {
        self.last_keyframe = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_heartbeats_do_not_force_one_second_idrs() {
        let start = Instant::now();
        let mut policy = KeyframePolicy::default();
        assert!(policy.periodic_due(start));
        policy.submitted(start, true);
        for second in 1..10 {
            let now = start + Duration::from_secs(second);
            assert!(policy.refresh_due(now, false));
            assert!(!policy.periodic_due(now));
            policy.submitted(now, false);
            assert!(!policy.refresh_due(now + Duration::from_millis(999), false));
        }
        assert!(policy.periodic_due(start + RECOVERY_INTERVAL));
    }

    #[test]
    fn feedback_is_prompt_on_static_sources_but_storms_are_coalesced() {
        let start = Instant::now();
        let mut policy = KeyframePolicy::default();
        policy.submitted(start, true);
        assert!(!policy.refresh_due(start + Duration::from_millis(199), true));
        let now = start + FEEDBACK_INTERVAL;
        assert!(policy.feedback_ready(now));
        assert!(policy.refresh_due(now, true));
        // A busy encoder must leave the same request eligible on the next poll.
        assert!(policy.refresh_due(now + Duration::from_millis(1), true));
        policy.submitted(now, true);
        assert!(!policy.feedback_ready(now + Duration::from_millis(199)));
        assert!(policy.feedback_ready(now + FEEDBACK_INTERVAL));
    }

    #[test]
    fn automatic_idr_resets_fallback_and_new_encoder_recovers_immediately() {
        let start = Instant::now();
        let mut policy = KeyframePolicy::default();
        policy.submitted(start, true);
        policy.observed(start + Duration::from_secs(9));
        assert!(!policy.periodic_due(start + RECOVERY_INTERVAL));
        assert!(policy.periodic_due(start + Duration::from_secs(19)));
        // Pause/resume, quality and dimension changes create a fresh policy.
        assert!(KeyframePolicy::default().periodic_due(start));
    }

    #[test]
    fn moving_input_only_needs_ten_second_fallback_at_every_supported_rate() {
        for fps in [5, 15, 30, 60] {
            let start = Instant::now();
            let mut policy = KeyframePolicy::default();
            let mut forced = 0;
            for frame in 0..gop_frames(fps) {
                let now = start + Duration::from_secs_f64(f64::from(frame) / f64::from(fps));
                let force = policy.periodic_due(now);
                forced += u32::from(force);
                policy.submitted(now, force);
                assert!(!policy.refresh_due(now, false));
            }
            assert_eq!(forced, 1);
            assert!(policy.periodic_due(start + RECOVERY_INTERVAL));
        }
    }
}
