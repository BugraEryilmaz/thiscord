//! Voice authorization epochs and a mutation-only media drain.
//!
//! Normal authorization does not acquire a global lock. Packets take an atomic
//! permit; an access-changing blocking DB operation closes admission and drains
//! existing permits BEFORE it can commit. Its RAII guard survives HTTP task
//! cancellation because it lives inside the blocking operation, not the handler.
//! Old DB snapshots and queued packets cannot be promoted into the new epoch.
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
};
use tokio::sync::watch;

pub(crate) fn global() -> &'static Arc<Access> {
    static ACCESS: OnceLock<Arc<Access>> = OnceLock::new();
    ACCESS.get_or_init(Access::new)
}

pub(crate) struct Access {
    epoch: AtomicU64,
    mutations: AtomicUsize,
    packets: AtomicUsize,
    drained: Condvar,
    waiting: Mutex<()>,
    changed: watch::Sender<u64>,
}
impl Access {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: AtomicU64::new(0),
            mutations: AtomicUsize::new(0),
            packets: AtomicUsize::new(0),
            drained: Condvar::new(),
            waiting: Mutex::new(()),
            changed: watch::channel(0).0,
        })
    }
    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub(super) fn snapshot(&self) -> Option<u64> {
        let epoch = self.epoch.load(SeqCst);
        (self.mutations.load(SeqCst) == 0 && self.epoch.load(SeqCst) == epoch).then_some(epoch)
    }
    pub(super) fn current(&self, epoch: u64) -> bool {
        self.snapshot() == Some(epoch)
    }

    /// Nonblocking admission. SeqCst provides a single order between a writer
    /// closing admission and a reader registering/rechecking its epoch. A reader
    /// missed by the writer's drain necessarily sees closed/changed admission.
    pub(super) fn packet(self: &Arc<Self>, queued: u64, authorized: u64) -> Option<Packet> {
        if queued != authorized || !self.current(authorized) {
            return None;
        }
        self.packets.fetch_add(1, SeqCst);
        let permit = Packet(self.clone());
        self.current(authorized).then_some(permit)
    }

    /// Only call on a blocking DB worker, and retain through commit/rollback.
    /// Never call while holding a packet permit. No DB or room locks are held
    /// by packet permits across a DB operation, so draining cannot depend on the
    /// caller releasing a transaction. Nested/concurrent mutations are supported.
    pub(crate) fn pause(self: &Arc<Self>) -> Mutation {
        self.mutations.fetch_add(1, SeqCst);
        self.epoch.fetch_add(1, SeqCst);
        let mutation = Mutation(self.clone());
        let mut waiting = self.waiting.lock().unwrap();
        while self.packets.load(SeqCst) != 0 {
            waiting = self.drained.wait(waiting).unwrap();
        }
        mutation
    }

    /// Check DB state without a global guard, retrying if an access mutation
    /// overlaps it. Failed snapshots are retried too: they may predate a grant.
    pub(super) async fn authorize<T, E, F, Fut>(
        &self,
        changed: &mut watch::Receiver<u64>,
        mut query: F,
    ) -> Result<(T, u64), E>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        loop {
            changed.borrow_and_update();
            let Some(epoch) = self.snapshot() else {
                // Sender lives with self and cannot close during this borrow.
                let _ = changed.changed().await;
                continue;
            };
            let result = query().await;
            if self.current(epoch) {
                return result.map(|value| (value, epoch));
            }
        }
    }
}

pub(super) struct Packet(Arc<Access>);
impl Drop for Packet {
    fn drop(&mut self) {
        if self.0.packets.fetch_sub(1, SeqCst) == 1 && self.0.mutations.load(SeqCst) != 0 {
            // Only the last in-flight packet during a mutation needs the mutex;
            // ordinary packet forwarding takes no mutex or async RwLock.
            let _waiting = self.0.waiting.lock().unwrap();
            self.0.drained.notify_all();
        }
    }
}
pub(crate) struct Mutation(Arc<Access>);
impl Drop for Mutation {
    fn drop(&mut self) {
        // Even rollback invalidates snapshots taken during the attempt. The
        // last concurrent mutation reopens admission, but every client must
        // independently validate this final epoch against the DB first.
        self.0.epoch.fetch_add(1, SeqCst);
        self.0.mutations.fetch_sub(1, SeqCst);
        self.0.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn mutation_drains_in_flight_packets_and_rejects_stale_epochs() {
        let access = Access::new();
        let epoch = access.snapshot().unwrap();
        let packet = access.packet(epoch, epoch).unwrap();
        let other = access.clone();
        let (entered, wait_entered) = std::sync::mpsc::channel();
        let (finish, wait_finish) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _mutation = other.pause();
            entered.send(()).unwrap();
            wait_finish.recv().unwrap();
        });
        while access.mutations.load(SeqCst) == 0 {
            std::thread::yield_now();
        }
        assert!(access.packet(epoch, epoch).is_none());
        assert!(
            wait_entered.try_recv().is_err(),
            "commit cannot start with a packet in flight"
        );
        drop(packet);
        wait_entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(access.snapshot().is_none());
        finish.send(()).unwrap();
        writer.join().unwrap();
        let next = access.snapshot().unwrap();
        assert_ne!(next, epoch);
        assert!(
            access.packet(epoch, next).is_none(),
            "queued old data cannot acquire new permission"
        );
        assert!(
            access.packet(next, epoch).is_none(),
            "old permission cannot admit new data"
        );
        assert!(access.packet(next, next).is_some());
    }

    #[test]
    fn nested_mutations_and_rollback_cannot_reopen_admission_early() {
        let access = Access::new();
        let epoch = access.snapshot().unwrap();
        let a = access.pause();
        let b = access.pause();
        drop(a);
        assert!(access.snapshot().is_none());
        drop(b);
        assert!(access.snapshot().is_some());
        assert!(access.packet(epoch, epoch).is_none());
    }

    #[tokio::test]
    async fn cancelling_handler_does_not_release_blocking_workers_mutation() {
        let access = Access::new();
        let worker_access = access.clone();
        let (entered, wait_entered) = tokio::sync::oneshot::channel();
        let (finish, wait_finish) = std::sync::mpsc::channel();
        let (finished, wait_finished) = tokio::sync::oneshot::channel();
        let handler = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let mutation = worker_access.pause();
                entered.send(()).unwrap();
                wait_finish.recv().unwrap();
                drop(mutation);
                finished.send(()).unwrap();
            })
            .await
            .unwrap();
        });
        wait_entered.await.unwrap();
        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        assert!(access.snapshot().is_none());
        finish.send(()).unwrap();
        wait_finished.await.unwrap();
        assert!(access.snapshot().is_some());
    }

    #[tokio::test]
    async fn delayed_authorization_does_not_stop_media_and_cannot_publish_stale_success() {
        let access = Access::new();
        let epoch = access.snapshot().unwrap();
        let mut changed = access.subscribe();
        let mut calls = 0;
        let result = access
            .authorize(&mut changed, || {
                calls += 1;
                let access = access.clone();
                let call = calls;
                async move {
                    if call == 1 {
                        assert!(
                            access.packet(epoch, epoch).is_some(),
                            "DB check must not block forwarding"
                        );
                        let writer = access.clone();
                        tokio::task::spawn_blocking(move || drop(writer.pause()))
                            .await
                            .unwrap();
                        Ok("old grant")
                    } else {
                        Err("revoked")
                    }
                }
            })
            .await;
        assert_eq!(result, Err("revoked"));
        assert_eq!(calls, 2);
    }
}
