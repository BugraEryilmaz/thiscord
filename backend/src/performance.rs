//! Fixed-size process counters: no SQL, parameters, identities or payloads.
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::{Duration, Instant},
};
use thiscord_shared::performance::{BackendPerformance, Measurement, Timing};

pub(crate) struct Metric {
    name: &'static str,
    budget_us: u64,
    calls: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    over_budget: AtomicU64,
}
impl Metric {
    const fn new(name: &'static str, budget_us: u64) -> Self {
        Self {
            name,
            budget_us,
            calls: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            over_budget: AtomicU64::new(0),
        }
    }
    pub(crate) fn start(&self) -> Timer<'_> {
        Timer {
            metric: self,
            start: Instant::now(),
        }
    }
    pub(crate) fn observe(&self, elapsed: Duration) {
        let us = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.total_us.fetch_add(us, Relaxed);
        self.max_us.fetch_max(us, Relaxed);
        if us > self.budget_us {
            self.over_budget.fetch_add(1, Relaxed);
        }
        self.calls.fetch_add(1, Relaxed);
    }
    fn snapshot(&self) -> Measurement {
        Measurement {
            name: self.name.into(),
            budget_us: self.budget_us,
            timing: Timing {
                calls: self.calls.load(Relaxed),
                total_us: self.total_us.load(Relaxed),
                max_us: self.max_us.load(Relaxed),
                over_budget: self.over_budget.load(Relaxed),
            },
        }
    }
}
pub(crate) struct Timer<'a> {
    metric: &'a Metric,
    start: Instant,
}
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        self.metric.observe(self.start.elapsed());
    }
}

pub(crate) static POOL: Metric = Metric::new("Database pool checkout", 10_000);
pub(crate) static QUERY: Metric = Metric::new("SQL query + JSON decode", 50_000);
pub(crate) static EXECUTE: Metric = Metric::new("SQL execute / locks", 50_000);
pub(crate) static BLOCKING: Metric = Metric::new("Chat/friends/notification worker queue", 10_000);
pub(crate) static ROSTER: Metric = Metric::new("Voice roster authorization", 50_000);
pub(crate) static FRIENDS: Metric = Metric::new("Friends snapshot", 50_000);
pub(crate) static REFRESH: Metric = Metric::new("Chat refresh", 50_000);
pub(crate) static REFRESH_GRANTED: Metric =
    Metric::new("Chat refresh (reused authorization)", 50_000);
pub(crate) static UNREAD: Metric = Metric::new("Unread authorization + counts", 50_000);
pub(crate) static AUTHORIZATION: Metric = Metric::new("Focused permission inputs", 20_000);
pub(crate) static ADMIN: Metric = Metric::new("Admin diagnostics collection", 200_000);
pub(crate) static SFU_QUEUE: Metric = Metric::new("SFU ingress queue age (1/64 samples)", 10_000);
pub(crate) static SFU_LOCK: Metric = Metric::new("SFU room read wait (1/64 samples)", 1_000);
pub(crate) static SFU_FANOUT: Metric = Metric::new("SFU locked fanout (1/64 samples)", 2_000);
pub(crate) static POOL_FAILURES: AtomicU64 = AtomicU64::new(0);
pub(crate) static ROSTER_UNCHANGED: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRIENDS_UNCHANGED: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRIENDS_SKIPPED: AtomicU64 = AtomicU64::new(0);

/// Runs socket and maintenance database work on a blocking thread. Callers
/// first queue asynchronously for one of as many permits as the pool has
/// connections, so a fan-out of hundreds of sockets parks tasks, not blocking
/// threads waiting for a connection. The permit lives until the work ends, even
/// if the caller is cancelled. The queue time counts toward this measurement.
pub(crate) async fn blocking<F, R>(f: F) -> Result<R, tokio::task::JoinError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let queued = Instant::now();
    let permit = database_permits()
        .clone()
        .acquire_owned()
        .await
        .expect("database permits are never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        BLOCKING.observe(queued.elapsed());
        f()
    })
    .await
}

/// One permit per pool connection (`DATABASE_POOL_SIZE`, default 5).
fn database_permits() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    PERMITS.get_or_init(|| {
        let size = crate::db::pool_size(std::env::var(crate::db::POOL_SIZE_VAR).ok().as_deref())
            .unwrap_or(crate::db::DEFAULT_POOL_SIZE);
        std::sync::Arc::new(tokio::sync::Semaphore::new(size as usize))
    })
}

pub(crate) fn snapshot() -> BackendPerformance {
    BackendPerformance {
        timings: [
            &POOL,
            &QUERY,
            &EXECUTE,
            &BLOCKING,
            &ROSTER,
            &FRIENDS,
            &REFRESH,
            &REFRESH_GRANTED,
            &UNREAD,
            &AUTHORIZATION,
            &ADMIN,
            &SFU_QUEUE,
            &SFU_LOCK,
            &SFU_FANOUT,
        ]
        .into_iter()
        .map(Metric::snapshot)
        .collect(),
        pool_failures: POOL_FAILURES.load(Relaxed),
        roster_unchanged: ROSTER_UNCHANGED.load(Relaxed),
        friends_unchanged: FRIENDS_UNCHANGED.load(Relaxed),
        friends_admission_skipped: FRIENDS_SKIPPED.load(Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_concurrent_samples_and_strict_budget() {
        let metric = Metric::new("test", 100);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for us in [0, 100, 101] {
                        metric.observe(Duration::from_micros(us));
                    }
                });
            }
        });
        let t = metric.snapshot().timing;
        assert_eq!(
            (t.calls, t.total_us, t.max_us, t.over_budget),
            (12, 804, 101, 4)
        );
    }
    #[test]
    fn guard_records_early_return() {
        let metric = Metric::new("test", 100);
        let fail = || {
            let _timer = metric.start();
            Err::<(), ()>(())
        };
        assert!(fail().is_err());
        assert_eq!(metric.snapshot().timing.calls, 1);
    }
}
