use std::time::{Duration, Instant};

/// Throttle compositor events without sleeping or converting tiny cadence jitter
/// into a half-rate stream. Overload never schedules a burst of catch-up frames.
pub struct Cadence {
    period: Duration,
    next: Option<Instant>,
}
impl Cadence {
    pub fn new(period: Duration) -> Self {
        Self { period, next: None }
    }
    /// Checking admission must not consume a slot when no input is available or
    /// downstream is busy. Commit with `accept` only when work is submitted.
    pub fn ready(&self, now: Instant) -> bool {
        self.next
            .is_none_or(|at| now + Duration::from_millis(1) >= at)
    }
    pub fn accept(&mut self, now: Instant) -> bool {
        if !self.ready(now) {
            return false;
        }
        self.next = Some(match self.next {
            Some(at) if now.saturating_duration_since(at) <= self.period => at + self.period,
            _ => now + self.period,
        });
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_polling_and_busy_encoder_do_not_consume_capture_slots() {
        // A 5 FPS capture arrives between encoder polls, with a gap for a static
        // source and a delayed first NeedInput event after reconfiguration.
        // Advancing on every poll used to hold each input for almost 200 ms.
        let start = Instant::now();
        let mut cadence = Cadence::new(Duration::from_millis(200));
        let mut pending = None;
        let mut ages = Vec::new();
        for ms in 0..2400 {
            if ms % 200 == 40 && !(800..1400).contains(&ms) {
                pending = Some(ms);
            }
            let now = start + Duration::from_millis(ms);
            if ms % 3 != 0 || !cadence.ready(now) {
                continue;
            }
            // Polling without pixels/encoder credit leaves the slot available.
            if ms < 50 {
                continue;
            }
            if let Some(captured) = pending.take() {
                assert!(cadence.accept(now));
                ages.push(ms - captured);
            }
        }
        assert_eq!(ages.len(), 9);
        assert!(ages.into_iter().all(|age| age <= 12));
    }
    #[test]
    fn compositor_jitter_does_not_halve_sixty_fps() {
        let start = Instant::now();
        let mut cadence = Cadence::new(Duration::from_secs_f64(1.0 / 60.0));
        let count = (0..600)
            .filter(|i| cadence.accept(start + Duration::from_secs_f64(*i as f64 / 59.94)))
            .count();
        assert_eq!(count, 600);
    }
    #[test]
    fn fast_sources_are_capped_and_stalls_do_not_catch_up() {
        let start = Instant::now();
        let mut cadence = Cadence::new(Duration::from_secs_f64(1.0 / 30.0));
        let count = (0..1440)
            .filter(|i| cadence.accept(start + Duration::from_secs_f64(*i as f64 / 144.0)))
            .count();
        assert!((299..=301).contains(&count));
        let resumed = start + Duration::from_secs(20);
        assert!(cadence.accept(resumed));
        assert!(!cadence.accept(resumed + Duration::from_millis(2)));
    }
}
