//! Scoped connection authorization epochs and mutation-only media drains.
//!
//! Normal authorization does not acquire a global lock. Packets take an atomic
//! permit; an access-changing blocking DB operation closes matching admission and drains
//! existing permits BEFORE it can commit. Its RAII guard survives HTTP task
//! cancellation because it lives inside the blocking operation, not the handler.
//! Old DB snapshots and queued packets cannot be promoted into the new epoch.
use std::sync::{
    Arc, Condvar, Mutex, OnceLock, Weak,
    atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
};
use tokio::sync::watch;

pub(crate) struct Access {
    epoch: AtomicU64,
    mutations: AtomicUsize,
    packets: AtomicUsize,
    drained: Condvar,
    waiting: Mutex<()>,
    changed: watch::Sender<u64>,
    revoked: watch::Sender<u64>,
}
impl Access {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: AtomicU64::new(0),
            mutations: AtomicUsize::new(0),
            packets: AtomicUsize::new(0),
            drained: Condvar::new(),
            waiting: Mutex::new(()),
            changed: watch::channel(0).0,
            revoked: watch::channel(0).0,
        })
    }
    #[cfg(test)]
    pub(crate) fn revocations(&self) -> watch::Receiver<u64> {
        self.revoked.subscribe()
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub(crate) fn snapshot(&self) -> Option<u64> {
        let epoch = self.epoch.load(SeqCst);
        (self.mutations.load(SeqCst) == 0 && self.epoch.load(SeqCst) == epoch).then_some(epoch)
    }
    pub(crate) fn current(&self, epoch: u64) -> bool {
        self.snapshot() == Some(epoch)
    }

    /// Nonblocking admission. SeqCst provides a single order between a writer
    /// closing admission and a reader registering/rechecking its epoch. A reader
    /// missed by the writer's drain necessarily sees closed/changed admission.
    pub(crate) fn packet(self: &Arc<Self>, queued: u64, authorized: u64) -> Option<Packet> {
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
    fn begin(self: &Arc<Self>) -> Drain {
        self.mutations.fetch_add(1, SeqCst);
        self.epoch.fetch_add(1, SeqCst);
        Drain(self.clone())
    }
    fn drain(&self) {
        let mut waiting = self.waiting.lock().unwrap();
        while self.packets.load(SeqCst) != 0 {
            waiting = self.drained.wait(waiting).unwrap();
        }
    }
    #[cfg(test)]
    fn pause(self: &Arc<Self>) -> Drain {
        let guard = self.begin();
        self.drain();
        guard
    }

    /// Check DB state without a global guard, retrying if an access mutation
    /// overlaps it. Failed snapshots are retried too: they may predate a grant.
    pub(crate) async fn authorize<T, E, F, Fut>(
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

pub(crate) struct Packet(Arc<Access>);
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
struct Drain(Arc<Access>);
impl Drop for Drain {
    fn drop(&mut self) {
        // Even rollback invalidates snapshots taken during the attempt. The
        // last concurrent mutation reopens admission, but every client must
        // independently validate this final epoch against the DB first.
        self.0.epoch.fetch_add(1, SeqCst);
        self.0.mutations.fetch_sub(1, SeqCst);
        self.0.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
}

use thiscord_shared::{AccountId, GuildId, SessionId};

#[derive(Clone, Copy)]
pub(crate) struct Identity {
    pub account: AccountId,
    pub session: SessionId,
    pub guild: Option<GuildId>,
}
#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Account(AccountId),
    Session(SessionId),
    Guild(GuildId),
    Member(GuildId, AccountId),
}
impl Scope {
    fn matches(self, identity: Identity) -> bool {
        match self {
            Self::Account(id) => identity.account == id,
            Self::Session(id) => identity.session == id,
            Self::Guild(id) => identity.guild == Some(id),
            Self::Member(guild, account) => {
                identity.guild == Some(guild) && identity.account == account
            }
        }
    }
}
#[derive(Default)]
struct Registry {
    connections: Vec<(Identity, Weak<Access>)>,
    mutations: std::collections::HashMap<u64, (Scope, Vec<Drain>)>,
    next: u64,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
/// Identity discovery is not an authorization grant. Callers must validate again
/// after registration, using `authorize` for voice or the chat delivery gate.
pub(crate) async fn identify(
    pool: &crate::db::DbPool,
    token: &str,
    guild: Option<GuildId>,
) -> Result<(Identity, Arc<Access>), crate::auth::Failure> {
    let pool = pool.clone();
    let token = token.to_owned();
    let session = tokio::task::spawn_blocking(move || {
        crate::auth::store::authenticate(&mut *crate::auth::store::connection(&pool)?, &token)
    })
    .await
    .map_err(|_| crate::auth::Failure::Unavailable)??;
    let identity = Identity {
        account: session.account_id,
        session: session.id,
        guild,
    };
    Ok((identity, register(identity)))
}
/// Register before the authoritative DB check. Admission inherits every overlapping
/// mutation, including ones already draining, so joining cannot miss revocation.
pub(crate) fn register(identity: Identity) -> Arc<Access> {
    let access = Access::new();
    let mut registry = registry().lock().unwrap();
    registry
        .connections
        .retain(|(_, access)| access.strong_count() > 0);
    for (scope, drains) in registry.mutations.values_mut() {
        if scope.matches(identity) {
            drains.push(access.begin());
        }
    }
    registry
        .connections
        .push((identity, Arc::downgrade(&access)));
    access
}
/// Retain on the blocking worker through commit/rollback, never across a packet
/// permit. The registry lock is not held while draining or executing DB work.
pub(crate) fn pause(scope: Scope) -> Mutation {
    let mut registry = registry().lock().unwrap();
    let targets: Vec<_> = registry
        .connections
        .iter()
        .filter(|(identity, _)| scope.matches(*identity))
        .filter_map(|(_, access)| access.upgrade())
        .collect();
    let drains = targets.iter().map(|access| access.begin()).collect();
    let id = registry.next;
    registry.next = registry
        .next
        .checked_add(1)
        .expect("mutation counter exhausted");
    registry.mutations.insert(id, (scope, drains));
    drop(registry);
    let mutation = Mutation {
        id,
        committed: false,
        chat: Some(
            match scope {
                Scope::Account(id) => crate::chat::access::account(id),
                Scope::Session(id) => crate::chat::access::session(id),
                Scope::Guild(id) => crate::chat::access::guild(id),
                Scope::Member(guild, account) => crate::chat::access::member(guild, account),
            }
            .pause(),
        ),
    };
    for target in targets {
        target.drain();
    }
    mutation
}
pub(crate) struct Mutation {
    id: u64,
    committed: bool,
    chat: Option<crate::chat::access::Mutation>,
}
impl Mutation {
    pub(crate) fn finish(mut self, committed: bool) {
        self.committed = committed;
    }
    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}
impl Drop for Mutation {
    fn drop(&mut self) {
        if let Some(chat) = self.chat.take() {
            chat.finish(self.committed);
        }
        let mut registry = registry().lock().unwrap();
        let (_, drains) = registry
            .mutations
            .remove(&self.id)
            .expect("registered mutation");
        for drain in drains {
            if self.committed {
                drain.0.revoked.send_modify(|v| *v = v.wrapping_add(1));
            }
            drop(drain);
        }
    }
}
/// Publish only successful commits. Rollbacks still discard stale voice epochs.
pub(crate) fn complete<T, E>(result: Result<T, E>, mutations: &mut [Mutation]) -> Result<T, E> {
    if result.is_ok() {
        for mutation in mutations {
            mutation.commit();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Identity {
        Identity {
            account: AccountId::from_uuid(uuid::Uuid::new_v4()),
            session: SessionId::from_uuid(uuid::Uuid::new_v4()),
            guild: Some(GuildId::from_uuid(uuid::Uuid::new_v4())),
        }
    }

    #[test]
    fn scopes_isolate_media_epochs_and_chat_notifications() {
        let a = identity();
        let b = identity();
        let identities = [
            a,
            Identity {
                session: b.session,
                ..a
            },
            Identity {
                guild: b.guild,
                ..a
            },
            Identity {
                guild: a.guild,
                ..b
            },
            b,
            Identity { guild: None, ..a },
        ];
        let cases = [
            (
                Scope::Session(a.session),
                [true, false, true, false, false, true],
            ),
            (
                Scope::Account(a.account),
                [true, true, true, false, false, true],
            ),
            (
                Scope::Guild(a.guild.unwrap()),
                [true, true, false, true, false, false],
            ),
            (
                Scope::Member(a.guild.unwrap(), a.account),
                [true, true, false, false, false, false],
            ),
        ];
        for (scope, affected) in cases {
            let connections: Vec<_> = identities.into_iter().map(register).collect();
            let notifications: Vec<_> = connections
                .iter()
                .map(|a| (a.subscribe(), a.revocations()))
                .collect();
            // Held unrelated permits must never delay the affected mutation.
            let unrelated: Vec<_> = connections
                .iter()
                .zip(affected)
                .filter(|(_, affected)| !affected)
                .map(|(a, _)| a.packet(0, 0).unwrap())
                .collect();
            let (entered, waiting) = std::sync::mpsc::channel();
            let (finish, finished) = std::sync::mpsc::channel();
            let writer = std::thread::spawn(move || {
                let mut mutation = pause(scope);
                entered.send(()).unwrap();
                finished.recv().unwrap();
                mutation.commit();
            });
            waiting
                .recv_timeout(Duration::from_secs(2))
                .expect("unrelated media blocked mutation");
            for (a, affected) in connections.iter().zip(affected) {
                assert_eq!(a.packet(0, 0).is_none(), affected);
            }
            finish.send(()).unwrap();
            writer.join().unwrap();
            for ((a, (voice, chat)), affected) in
                connections.iter().zip(&notifications).zip(affected)
            {
                assert_eq!(voice.has_changed().unwrap(), affected);
                assert_eq!(chat.has_changed().unwrap(), affected);
                assert_eq!(
                    a.packet(0, 0).is_none(),
                    affected,
                    "old queued data survived"
                );
            }
            drop(unrelated);
        }
    }

    #[test]
    fn joining_during_overlapping_mutations_inherits_pause_and_commit_notification() {
        let id = identity();
        let mut account = pause(Scope::Account(id.account));
        let guild = pause(Scope::Guild(id.guild.unwrap()));
        let joined = register(id);
        let chat = joined.revocations();
        assert!(joined.snapshot().is_none());
        account.commit();
        drop(account);
        assert!(chat.has_changed().unwrap());
        assert!(
            joined.snapshot().is_none(),
            "overlapping rollback still in progress"
        );
        drop(guild);
        assert!(joined.snapshot().is_some());
        assert!(joined.packet(0, 0).is_none());
    }

    #[test]
    fn rollback_reauthorizes_only_matching_voice_without_revoking_chat() {
        let id = identity();
        let a = register(id);
        let voice = a.subscribe();
        let chat = a.revocations();
        drop(pause(Scope::Account(id.account)));
        assert!(voice.has_changed().unwrap());
        assert!(!chat.has_changed().unwrap());
        assert!(a.packet(0, 0).is_none());
    }

    #[tokio::test]
    async fn unrelated_mutation_does_not_retry_authorization() {
        let a = register(identity());
        let b = identity();
        let mut changes = a.subscribe();
        let mut calls = 0;
        let result = a
            .authorize(&mut changes, || {
                calls += 1;
                async {
                    drop(pause(Scope::Account(b.account)));
                    Ok::<_, ()>(())
                }
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(calls, 1);
    }

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
