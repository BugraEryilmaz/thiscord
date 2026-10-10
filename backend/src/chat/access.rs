//! Scoped delivery barriers. Database work never holds a delivery permit:
//! capture an epoch before querying, then validate it when admitting a send.
//! Successful mutations drain admitted sends before commit and survive through it on
//! the blocking worker. Database locks still serialize authorization and writes.
use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
    },
};
use thiscord_shared::{AccountId, GuildId};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Account(AccountId),
    Guild(GuildId),
    InstanceRole(AccountId),
}

fn scope(key: Key) -> Arc<Access> {
    static SCOPES: OnceLock<Mutex<HashMap<Key, Weak<Access>>>> = OnceLock::new();
    let mut scopes = SCOPES.get_or_init(Default::default).lock().unwrap();
    if let Some(access) = scopes.get(&key).and_then(Weak::upgrade) {
        return access;
    }
    // Only live sockets and mutations retain entries; attacker-chosen IDs cannot
    // grow this registry indefinitely. Never hold the registry during DB or I/O.
    scopes.retain(|_, value| value.strong_count() != 0);
    let access = Arc::new(Access {
        epoch: AtomicU64::new(0),
        mutations: AtomicUsize::new(0),
        sends: AtomicUsize::new(0),
        drained: Condvar::new(),
        waiting: Mutex::new(()),
        changed: watch::channel(0).0,
    });
    scopes.insert(key, Arc::downgrade(&access));
    access
}
pub(crate) fn account(id: AccountId) -> Arc<Access> {
    scope(Key::Account(id))
}
// Instance privilege affects diagnostics, never guild/chat access.
pub(crate) fn instance_role(id: AccountId) -> Arc<Access> {
    scope(Key::InstanceRole(id))
}
pub(crate) fn guild(id: GuildId) -> Arc<Access> {
    scope(Key::Guild(id))
}

pub(crate) struct Access {
    epoch: AtomicU64,
    mutations: AtomicUsize,
    sends: AtomicUsize,
    drained: Condvar,
    waiting: Mutex<()>,
    changed: watch::Sender<u64>,
}
impl Access {
    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    fn current(&self, epoch: u64) -> bool {
        self.mutations.load(SeqCst) == 0 && self.epoch.load(SeqCst) == epoch
    }
    /// Blocking worker only. Concurrent/nested mutations do not wait on each
    /// other, so draining inside a DB transaction cannot reverse DB lock order.
    /// Pause after successful authorization/writes, immediately before commit;
    /// uncommitted writes are invisible and rejected commands never pause sends.
    pub(crate) fn pause(self: &Arc<Self>) -> Mutation {
        self.mutations.fetch_add(1, SeqCst);
        let mut waiting = self.waiting.lock().unwrap();
        while self.sends.load(SeqCst) != 0 {
            waiting = self.drained.wait(waiting).unwrap();
        }
        Mutation(self.clone())
    }
}
pub(crate) struct Mutation(Arc<Access>);
impl Mutation {
    pub(crate) fn finish(self, committed: bool) {
        if committed {
            self.0.epoch.fetch_add(1, SeqCst);
            self.0.changed.send_modify(|v| *v = v.wrapping_add(1));
        }
    }
}
impl Drop for Mutation {
    fn drop(&mut self) {
        self.0.mutations.fetch_sub(1, SeqCst);
    }
}

pub(crate) struct Snapshot(Vec<(Arc<Access>, u64)>);
pub(crate) fn snapshot(account: &Arc<Access>, resource: Option<&Arc<Access>>) -> Option<Snapshot> {
    let mut scopes = vec![account.clone()];
    scopes.extend(resource.cloned());
    scopes
        .into_iter()
        .map(|scope| {
            let epoch = scope.epoch.load(SeqCst);
            scope.current(epoch).then_some((scope, epoch))
        })
        .collect::<Option<Vec<_>>>()
        .map(Snapshot)
}
impl Snapshot {
    // SeqCst orders registration against mutation admission: either the writer
    // drains this send, or this send observes the active mutation/new epoch.
    pub(crate) fn deliver(self) -> Option<Vec<Delivery>> {
        self.0
            .into_iter()
            .map(|(scope, epoch)| {
                scope.sends.fetch_add(1, SeqCst);
                let delivery = Delivery(scope.clone());
                scope.current(epoch).then_some(delivery)
            })
            .collect()
    }
}
pub(crate) struct Delivery(Arc<Access>);
impl Drop for Delivery {
    fn drop(&mut self) {
        if self.0.sends.fetch_sub(1, SeqCst) == 1 && self.0.mutations.load(SeqCst) != 0 {
            let _waiting = self.0.waiting.lock().unwrap();
            self.0.drained.notify_all();
        }
    }
}

pub(super) async fn changed(receiver: &mut Option<watch::Receiver<u64>>) {
    match receiver {
        Some(receiver) => {
            let _ = receiver.changed().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uuid::Uuid;

    #[test]
    fn scoped_mutations_reject_stale_delivery_and_preserve_unrelated_scopes() {
        let a = account(AccountId::from_uuid(Uuid::new_v4()));
        let b = account(AccountId::from_uuid(Uuid::new_v4()));
        let g = guild(GuildId::from_uuid(Uuid::new_v4()));
        let old = snapshot(&a, Some(&g)).unwrap();
        let mut changed = a.subscribe();
        let mutation = a.pause();
        assert!(snapshot(&a, None).is_none());
        assert!(snapshot(&b, Some(&g)).unwrap().deliver().is_some());
        mutation.finish(true);
        assert!(old.deliver().is_none());
        assert!(changed.has_changed().unwrap());
        changed.borrow_and_update();
        let old = snapshot(&a, Some(&g)).unwrap();
        a.pause().finish(false);
        assert!(old.deliver().is_some());
        assert!(!changed.has_changed().unwrap());
        let old = snapshot(&b, Some(&g)).unwrap();
        g.pause().finish(true);
        assert!(old.deliver().is_none());
    }

    #[test]
    fn mutation_drains_sends_without_serializing_other_mutations() {
        let a = account(AccountId::from_uuid(Uuid::new_v4()));
        let delivery = snapshot(&a, None).unwrap().deliver().unwrap();
        let (started, waiting) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let worker_access = a.clone();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            let mutation = worker_access.pause();
            // Nested changes, such as replay detection, must not deadlock.
            worker_access.pause().finish(true);
            mutation.finish(true);
            finished.send(()).unwrap();
        });
        waiting.recv().unwrap();
        assert!(done.recv_timeout(Duration::from_millis(50)).is_err());
        drop(delivery);
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }
}
