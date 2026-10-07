//! Byte pacing with a small bounded burst and no catch-up after a scheduler stall.
use std::time::{Duration, Instant};
pub struct Pacer {
    next: Option<Instant>,
    bytes_per_second: u64,
}
impl Pacer {
    pub fn new(bitrate: u32) -> Self {
        Self {
            next: None,
            bytes_per_second: u64::from(bitrate) / 8,
        }
    }
    pub fn reserve(&mut self, now: Instant, bytes: usize) -> Instant {
        // At most 8 ms of burst credit amortizes OS timer resolution. A stall
        // never accumulates an entire frame/second of catch-up credit.
        let quantum = Duration::from_millis(8);
        let at = self.next.unwrap_or(now).max(now - quantum);
        self.next =
            Some(at + Duration::from_secs_f64(bytes as f64 / self.bytes_per_second.max(1) as f64));
        if at <= now {
            now
        } else {
            // Sleep once for a packet batch, not once per sub-millisecond
            // packet interval. Keep byte debt anchored to the original clock.
            now + quantum
                * (at
                    .duration_since(now)
                    .as_nanos()
                    .div_ceil(quantum.as_nanos()) as u32)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalls_do_not_release_a_catchup_burst() {
        let mut p = Pacer::new(8_000_000);
        let now = Instant::now();
        assert_eq!(p.reserve(now, 1000), now);
        assert_eq!(p.reserve(now, 1000), now + Duration::from_millis(8));
        let later = now + Duration::from_secs(1);
        assert_eq!(p.reserve(later, 1000), later);
        for _ in 0..8 {
            assert_eq!(p.reserve(later, 1000), later);
        }
        assert_eq!(p.reserve(later, 1000), later + Duration::from_millis(8));
    }
    #[test]
    fn batching_preserves_rate_with_coarse_wakeups() {
        let start = Instant::now();
        let mut now = start;
        let mut p = Pacer::new(8_000_000);
        let mut sleeps = 0;
        for _ in 0..1000 {
            let at = p.reserve(now, 1000);
            if at > now {
                sleeps += 1;
                now = at + Duration::from_millis(2);
            }
        }
        assert!(now.duration_since(start) >= Duration::from_millis(990));
        assert!(now.duration_since(start) <= Duration::from_millis(1010));
        assert!(
            sleeps < 150,
            "must batch instead of sleeping for every packet"
        );
    }
}
