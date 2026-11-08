//! Planned restarts. On SIGTERM the process stops admitting voice joins, tells
//! connected clients it is restarting (they rejoin after fresh admission once a
//! new process is ready), waits a bounded time for rooms to drain, then stops.
//!
//! The state belongs to one router, not the process, so integration tests can
//! drain one router while others keep serving.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// How long voice rooms may take to drain before the server stops anyway.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A process this young answers a resume for an unknown session with
/// `Restarting` (the session belonged to the previous process) and does not
/// announce automatic rejoins as fresh voice joins.
pub const REJOIN_WINDOW: Duration = Duration::from_secs(180);
/// Clients wait this long, plus up to `RETRY_JITTER_MS`, before the first rejoin,
/// so a room's members do not all reconnect in the same instant.
pub const RETRY_BASE_MS: u64 = 1_500;
pub const RETRY_JITTER_MS: u64 = 1_500;

#[derive(Clone)]
pub struct Shutdown(Arc<Inner>);

struct Inner {
    draining: watch::Sender<bool>,
    /// Live voice connections served by this router.
    connections: watch::Sender<usize>,
    started: Instant,
    rejoin_window: Duration,
    restarts: AtomicUsize,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::with_rejoin_window(Duration::ZERO)
    }
}

impl Shutdown {
    /// For a production process: it may follow a restart of the previous one.
    pub fn after_restart() -> Self {
        Self::with_rejoin_window(REJOIN_WINDOW)
    }

    pub fn with_rejoin_window(rejoin_window: Duration) -> Self {
        Self(Arc::new(Inner {
            draining: watch::channel(false).0,
            connections: watch::channel(0).0,
            started: Instant::now(),
            rejoin_window,
            restarts: AtomicUsize::new(0),
        }))
    }

    /// Enters the draining state. Returns false if it was already draining.
    pub fn begin(&self) -> bool {
        let first = !self.0.draining.send_replace(true);
        if first {
            tracing::info!(
                voice_connections = *self.0.connections.borrow(),
                "planned restart: draining voice and sockets"
            );
        }
        first
    }

    pub fn draining(&self) -> bool {
        *self.0.draining.borrow()
    }

    /// Resolves once draining starts (immediately if it already has).
    pub async fn drained(&self) {
        let mut draining = self.0.draining.subscribe();
        let _ = draining.wait_for(|draining| *draining).await;
    }

    /// Whether an automatic rejoin probably follows this process's start.
    pub fn rejoining(&self) -> bool {
        self.0.started.elapsed() < self.0.rejoin_window
    }

    /// Counts a voice connection until the returned guard drops.
    pub fn track(&self) -> Connection {
        self.0.connections.send_modify(|count| *count += 1);
        Connection(self.clone())
    }

    /// Records one notified voice connection, for the drain summary.
    pub(crate) fn notified(&self) {
        self.0.restarts.fetch_add(1, Ordering::Relaxed);
    }

    /// Waits until every voice connection has ended or `limit` passes.
    /// Returns how many connections were still open.
    pub async fn wait_idle(&self, limit: Duration) -> usize {
        let started = Instant::now();
        let mut connections = self.0.connections.subscribe();
        let _ = tokio::time::timeout(limit, connections.wait_for(|count| *count == 0)).await;
        let remaining = *connections.borrow();
        tracing::info!(
            notified = self.0.restarts.load(Ordering::Relaxed),
            remaining,
            elapsed_ms = started.elapsed().as_millis(),
            "voice drained for restart"
        );
        remaining
    }

    /// A per-connection hint for the first rejoin attempt.
    pub fn retry_after_ms() -> u64 {
        RETRY_BASE_MS + rand::random::<u64>() % (RETRY_JITTER_MS + 1)
    }
}

/// One tracked voice connection.
pub struct Connection(Shutdown);

impl Drop for Connection {
    fn drop(&mut self) {
        self.0
            .0
            .connections
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_waits_for_tracked_connections_with_a_bound() {
        let shutdown = Shutdown::default();
        assert!(!shutdown.draining() && !shutdown.rejoining());
        let first = shutdown.track();
        let second = shutdown.track();
        let waiting = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                shutdown.drained().await;
                shutdown.wait_idle(Duration::from_secs(5)).await
            }
        });
        assert!(shutdown.begin());
        assert!(!shutdown.begin(), "draining starts once");
        drop(first);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        drop(second);
        assert_eq!(waiting.await.unwrap(), 0);
        // A connection that never ends cannot hold the process past the bound.
        let stuck = shutdown.track();
        let started = Instant::now();
        assert_eq!(shutdown.wait_idle(Duration::from_millis(50)).await, 1);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(stuck);
    }

    #[test]
    fn retry_hints_are_jittered_within_bounds_and_rejoin_window_is_opt_in() {
        for _ in 0..64 {
            let hint = Shutdown::retry_after_ms();
            assert!((RETRY_BASE_MS..=RETRY_BASE_MS + RETRY_JITTER_MS).contains(&hint));
        }
        assert!(Shutdown::after_restart().rejoining());
        assert!(!Shutdown::default().rejoining());
    }
}
