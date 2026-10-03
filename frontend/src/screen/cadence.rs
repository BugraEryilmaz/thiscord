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
    pub fn accept(&mut self, now: Instant) -> bool {
        if self
            .next
            .is_some_and(|at| now + Duration::from_millis(1) < at)
        {
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
