//! Retrying long-lived connections' database work through transient trouble.
//!
//! Pool exhaustion, timeouts and lost connections say nothing about
//! authorization (`Failure::transient`). Sockets deliver nothing that needs the
//! failed read, retry after a jittered exponential delay, and give up only after
//! `GRACE` without a successful attempt, closing with a service error rather
//! than revocation.
use std::time::{Duration, Instant};

use rand::Rng;

/// How long trouble may last before a connection gives up.
pub(crate) const GRACE: Duration = Duration::from_secs(30);
const FIRST: Duration = Duration::from_millis(250);
const LONGEST: Duration = Duration::from_secs(4);

/// One connection's ongoing trouble, if any.
#[derive(Default)]
pub(crate) struct Backoff {
    since: Option<Instant>,
    attempts: u32,
}
impl Backoff {
    /// Records a failed attempt. Returns the delay before the next one, or
    /// `None` once the grace period is spent.
    pub(crate) fn failed(&mut self) -> Option<Duration> {
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() >= GRACE {
            return None;
        }
        let base = FIRST.saturating_mul(1 << self.attempts.min(5)).min(LONGEST);
        self.attempts = self.attempts.saturating_add(1);
        // Up to ±50% so many connections hit by the same outage spread out.
        Some(base.mul_f64(rand::thread_rng().gen_range(0.5..1.5)))
    }
    /// A successful attempt ends the trouble.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_with_jitter_up_to_a_cap_and_end_after_the_grace() {
        let mut backoff = Backoff::default();
        let delays: Vec<_> = (0..8).map(|_| backoff.failed().unwrap()).collect();
        let ms = Duration::from_millis;
        assert!(delays[0] >= ms(125) && delays[0] < ms(375), "{delays:?}");
        assert!(delays[1] >= ms(250) && delays[1] < ms(750), "{delays:?}");
        assert!(delays[2] >= ms(500) && delays[2] < ms(1500), "{delays:?}");
        assert!(delays.iter().all(|d| *d < ms(6000)), "{delays:?}");
        assert!(delays[7] >= ms(2000), "{delays:?}");
        backoff.since = Some(Instant::now() - GRACE);
        assert!(backoff.failed().is_none());
        backoff.clear();
        assert!(backoff.failed().unwrap() < ms(375));
    }
}
