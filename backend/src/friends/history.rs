//! Reuse of the selected conversation's history between friends snapshots.
//!
//! Each snapshot still authenticates, locks relationships and re-checks that the
//! selection is an accessible friendship; only the history query is skipped when
//! no message write to that conversation committed since it was loaded.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use thiscord_shared::{
    friends::{History, Snapshot},
    ids::ConversationId,
    pagination::PageCursor,
};

/// Defence in depth for writes that bypass `changed`, such as manual maintenance.
pub(super) const MAX_AGE: Duration = Duration::from_secs(60);
/// Fixed, lock-free write epochs. A shared shard only causes an extra reload.
const SHARDS: usize = 1024;
static EPOCHS: [AtomicU64; SHARDS] = [const { AtomicU64::new(0) }; SHARDS];

fn shard(id: ConversationId) -> &'static AtomicU64 {
    &EPOCHS[(id.as_uuid().as_u128() % SHARDS as u128) as usize]
}
/// Call after a committed message write in this conversation and before waking
/// sockets, like `friends::changed`. Carries no data or authorization.
pub(crate) fn changed(id: ConversationId) {
    shard(id).fetch_add(1, Ordering::AcqRel);
}
fn epoch(id: ConversationId) -> u64 {
    shard(id).load(Ordering::Acquire)
}

type Key = (ConversationId, Option<PageCursor>);
struct Loaded {
    key: Key,
    epoch: u64,
    at: Instant,
    history: History,
}
/// Per-socket state. Single process, like the socket wake-ups.
#[derive(Default)]
pub(super) struct Cache {
    loaded: Option<Loaded>,
    /// Increments when the loaded history's content changes.
    revision: u64,
    /// The revision this socket last sent for the loaded key.
    delivered: Option<u64>,
}
/// Captured before the snapshot transaction starts.
pub(super) struct Plan {
    epoch: Option<u64>,
    pub load: bool,
}
impl Cache {
    pub fn plan(&self, selected: Option<ConversationId>, before: &Option<PageCursor>) -> Plan {
        let epoch = selected.map(epoch);
        let reuse = self.loaded.as_ref().is_some_and(|l| {
            Some(l.key.0) == selected
                && l.key.1 == *before
                && Some(l.epoch) == epoch
                && l.at.elapsed() < MAX_AGE
        });
        Plan {
            epoch,
            load: !reuse,
        }
    }
    /// Moves a freshly loaded history into the cache. A snapshot without an
    /// authorized selection forgets it.
    pub fn finish(&mut self, plan: Plan, snapshot: &mut Snapshot) {
        let history = snapshot.history.take();
        let Some(id) = snapshot.conversation_id else {
            self.loaded = None;
            self.delivered = None;
            return;
        };
        let (Some(history), Some(epoch)) = (history, plan.epoch) else {
            return;
        };
        let key = (id, snapshot.before.clone());
        let same = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.key == key && l.history == history);
        if !same {
            self.revision += 1;
        }
        self.loaded = Some(Loaded {
            key,
            epoch,
            at: Instant::now(),
            history,
        });
    }
    /// A new selection always receives a full history.
    pub fn selected(&mut self) {
        self.delivered = None;
    }
    fn current(&self, snapshot: &Snapshot) -> Option<&History> {
        let id = snapshot.conversation_id?;
        self.loaded
            .as_ref()
            .filter(|l| l.key.0 == id && l.key.1 == snapshot.before)
            .map(|l| &l.history)
    }
    /// Whether the client lacks the current history revision.
    pub fn pending(&self, snapshot: &Snapshot) -> bool {
        self.current(snapshot).is_some() && self.delivered != Some(self.revision)
    }
    /// The snapshot to send. Clients that retain history receive it only when it
    /// changed; others receive it every time, as before.
    pub fn outgoing(&mut self, snapshot: &Snapshot, retains: bool) -> Snapshot {
        let pending = self.pending(snapshot);
        let mut out = snapshot.clone();
        out.history = self
            .current(snapshot)
            .filter(|_| pending || !retains)
            .cloned();
        if out.history.is_some() {
            self.delivered = Some(self.revision);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::{AccountId, ClientMessageId, MessageId, friends::DirectMessage};
    use uuid::Uuid;

    fn history(content: &str, conversation: ConversationId) -> History {
        let message = DirectMessage {
            id: MessageId::from_uuid(Uuid::from_u128(1)),
            conversation_id: conversation,
            client_id: ClientMessageId::from_uuid(Uuid::from_u128(2)),
            author_id: AccountId::from_uuid(Uuid::from_u128(3)),
            content: content.into(),
            created_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            revision: 1,
            deleted: false,
            edited_at: None,
            attachments: vec![],
            system: None,
        };
        History {
            messages: vec![message],
            older: None,
        }
    }
    fn loaded(id: ConversationId, history: History) -> Snapshot {
        Snapshot {
            conversation_id: Some(id),
            history: Some(history),
            ..Default::default()
        }
    }

    #[test]
    fn reloads_only_after_writes_selection_or_age_and_sends_only_changes() {
        let id = ConversationId::from_uuid(Uuid::new_v4());
        let mut cache = Cache::default();
        let plan = cache.plan(Some(id), &None);
        assert!(plan.load);
        let mut snapshot = loaded(id, history("one", id));
        cache.finish(plan, &mut snapshot);
        assert!(snapshot.history.is_none(), "history moves into the cache");
        // First delivery carries history for both kinds of clients.
        assert!(cache.pending(&snapshot));
        assert!(cache.outgoing(&snapshot, true).history.is_some());
        assert!(!cache.pending(&snapshot));
        assert!(cache.outgoing(&snapshot, true).history.is_none());
        assert!(cache.outgoing(&snapshot, false).history.is_some());

        // Unchanged epoch and selection: the history query is skipped.
        let plan = cache.plan(Some(id), &None);
        assert!(!plan.load);
        let mut reused = Snapshot {
            conversation_id: Some(id),
            ..Default::default()
        };
        cache.finish(plan, &mut reused);
        assert!(!cache.pending(&reused));

        // A committed write forces a reload; identical content is not resent.
        changed(id);
        let plan = cache.plan(Some(id), &None);
        assert!(plan.load);
        let mut same = loaded(id, history("one", id));
        cache.finish(plan, &mut same);
        assert!(cache.outgoing(&same, true).history.is_none());
        changed(id);
        let plan = cache.plan(Some(id), &None);
        let mut edited = loaded(id, history("two", id));
        cache.finish(plan, &mut edited);
        let sent = cache.outgoing(&edited, true).history.unwrap();
        assert_eq!(sent.messages[0].content, "two");

        // Another page or a new selection loads and delivers again.
        let page = Some(PageCursor::new(chrono::Utc::now(), Uuid::nil()));
        assert!(cache.plan(Some(id), &page).load);
        cache.selected();
        assert!(cache.outgoing(&edited, true).history.is_some());

        // Losing access forgets the cache and the delivery.
        let plan = cache.plan(Some(id), &None);
        cache.finish(plan, &mut Snapshot::default());
        assert!(cache.plan(Some(id), &None).load);
        assert!(cache.outgoing(&edited, true).history.is_none());
    }

    #[test]
    fn expired_cache_is_reloaded() {
        let id = ConversationId::from_uuid(Uuid::new_v4());
        let mut cache = Cache::default();
        let plan = cache.plan(Some(id), &None);
        cache.finish(plan, &mut loaded(id, history("one", id)));
        cache.loaded.as_mut().unwrap().at = Instant::now() - MAX_AGE;
        assert!(cache.plan(Some(id), &None).load);
    }
}
