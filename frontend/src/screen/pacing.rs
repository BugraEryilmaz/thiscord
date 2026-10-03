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
        // At most 3 ms of burst credit amortizes OS timer resolution. A stall
        // never accumulates an entire frame/second of catch-up credit.
        let at = self.next.unwrap_or(now).max(now - Duration::from_millis(3));
        self.next =
            Some(at + Duration::from_secs_f64(bytes as f64 / self.bytes_per_second.max(1) as f64));
        at.max(now)
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
        assert_eq!(p.reserve(now, 1000), now + Duration::from_millis(1));
        let later = now + Duration::from_secs(1);
        assert_eq!(p.reserve(later, 1000), later);
        assert_eq!(p.reserve(later, 1000), later);
        assert_eq!(p.reserve(later, 1000), later);
        assert_eq!(p.reserve(later, 1000), later);
        assert_eq!(p.reserve(later, 1000), later + Duration::from_millis(1));
    }
}
