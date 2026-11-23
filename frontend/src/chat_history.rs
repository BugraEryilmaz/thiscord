//! Bounded, ID-indexed message history in channel order, independent of the DOM.
use std::collections::{BTreeMap, HashMap};
use thiscord_shared::{AccountId, MessageId, Timestamp, chat::ChatMessage, pagination::PageCursor};

pub const RESIDENT_LIMIT: usize = 500;
/// Consecutive messages by one author within this window share a header.
pub const GROUP_WINDOW_SECONDS: i64 = 7 * 60;

/// Whether a message shares the previous message's author header: same known
/// author, sent in order and within [`GROUP_WINDOW_SECONDS`].
pub fn groups_with(
    previous_author: Option<AccountId>,
    previous_at: Timestamp,
    author: Option<AccountId>,
    at: Timestamp,
) -> bool {
    previous_author.is_some()
        && previous_author == author
        && (0..GROUP_WINDOW_SECONDS).contains(&(at - previous_at).num_seconds())
}

struct Entry<T> {
    value: T,
    revision: i32,
    sequence: i64,
    cursor: PageCursor,
}

pub struct MessageCache<T> {
    by_id: HashMap<MessageId, Entry<T>>,
    order: BTreeMap<i64, MessageId>,
    pub older: Option<PageCursor>,
    /// The resident window no longer reaches the live edge. Reload before marking read.
    pub newer: bool,
}

impl<T> Default for MessageCache<T> {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            order: BTreeMap::new(),
            older: None,
            newer: false,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Merge {
    Live { follow: bool },
    Older,
}

impl<T> MessageCache<T> {
    /// Resident rows in channel order, oldest first.
    pub fn ordered(&self) -> impl Iterator<Item = (MessageId, &T)> {
        self.order.values().map(|id| (*id, &self.by_id[id].value))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.by_id.values().map(|entry| &entry.value)
    }

    pub fn get(&self, id: MessageId) -> Option<&T> {
        self.by_id.get(&id).map(|entry| &entry.value)
    }

    pub fn latest_sequence(&self) -> i64 {
        self.order.last_key_value().map_or(0, |(seq, _)| *seq)
    }

    /// The resident row immediately before `id` in channel order.
    pub fn previous(&self, id: MessageId) -> Option<(MessageId, &T)> {
        let sequence = self.by_id.get(&id)?.sequence;
        let (_, previous) = self.order.range(..sequence).next_back()?;
        Some((*previous, &self.by_id[previous].value))
    }

    /// Newest resident row matching `predicate`.
    pub fn latest_where(&self, mut predicate: impl FnMut(&T) -> bool) -> Option<MessageId> {
        self.order
            .values()
            .rev()
            .find(|id| predicate(&self.by_id[*id].value))
            .copied()
    }

    /// One page/event batch, with O(log n) ordered insertion and O(1) ID lookup.
    /// `T` can be a reference-counted reactive row, released on eviction.
    pub fn merge(
        &mut self,
        messages: impl IntoIterator<Item = ChatMessage>,
        mode: Merge,
        create: impl Fn(ChatMessage) -> T,
        update: impl Fn(&mut T, ChatMessage),
    ) {
        for message in messages {
            if let Some(entry) = self.by_id.get_mut(&message.id) {
                if message.revision > entry.revision {
                    entry.revision = message.revision;
                    update(&mut entry.value, message);
                }
                continue;
            }
            if let Merge::Live { follow } = mode {
                // An edit of an evicted message must not resurrect an isolated row.
                if self
                    .order
                    .first_key_value()
                    .is_some_and(|(seq, _)| message.sequence < *seq)
                {
                    continue;
                }
                if message.sequence > self.latest_sequence()
                    && (self.newer || (!follow && self.by_id.len() == RESIDENT_LIMIT))
                {
                    self.newer = true;
                    continue;
                }
            }
            self.order.insert(message.sequence, message.id);
            self.by_id.insert(
                message.id,
                Entry {
                    revision: message.revision,
                    sequence: message.sequence,
                    cursor: PageCursor::new(message.created_at, message.id.as_uuid()),
                    value: create(message),
                },
            );
        }
        while self.by_id.len() > RESIDENT_LIMIT {
            let (_, id) = match mode {
                Merge::Older => {
                    self.newer = true;
                    self.order.pop_last().expect("nonempty cache")
                }
                Merge::Live { .. } => self.order.pop_first().expect("nonempty cache"),
            };
            self.by_id.remove(&id);
            if matches!(mode, Merge::Live { .. }) {
                // Evicted rows remain reachable through the existing keyset API.
                self.older = self
                    .order
                    .first_key_value()
                    .map(|(_, id)| self.by_id[id].cursor.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn message(sequence: i64) -> ChatMessage {
        let uuid = format!("00000000-0000-0000-0000-{sequence:012x}");
        ChatMessage {
            id: uuid.parse().unwrap(),
            client_id: uuid.parse().unwrap(),
            guild_id: uuid.parse().unwrap(),
            channel_id: uuid.parse().unwrap(),
            author_id: None,
            username: "test".into(),
            display_name: String::new(),
            author_avatar_id: None,
            content: format!("message {sequence}"),
            mentions: vec![],
            attachments: vec![],
            created_at: Timestamp::from_timestamp(sequence, 0).unwrap(),
            edited_at: None,
            deleted: false,
            revision: 1,
            sequence,
            reply: None,
            reactions: vec![],
            pinned: false,
        }
    }

    fn merge(cache: &mut MessageCache<ChatMessage>, messages: Vec<ChatMessage>, mode: Merge) {
        cache.merge(messages, mode, |m| m, |old, m| *old = m);
    }

    #[test]
    fn long_session_is_bounded_and_evicted_history_remains_reachable() {
        let mut cache = MessageCache::default();
        for sequence in 1..=10_000 {
            merge(
                &mut cache,
                vec![message(sequence)],
                Merge::Live { follow: true },
            );
            assert!(cache.by_id.len() <= RESIDENT_LIMIT);
            assert_eq!(cache.by_id.len(), cache.order.len());
        }
        assert_eq!(cache.latest_sequence(), 10_000);
        assert_eq!(
            cache.older.as_ref().unwrap().position(),
            (message(9501).created_at, message(9501).id.as_uuid())
        );
        assert!(cache.get(message(9500).id).is_none());
        assert!(!cache.newer);
        // An event for an evicted row must not reintroduce it or displace a resident row.
        let mut edited = message(20);
        edited.revision = 2;
        merge(&mut cache, vec![edited], Merge::Live { follow: true });
        assert!(cache.get(message(20).id).is_none());
        assert_eq!(cache.by_id.len(), RESIDENT_LIMIT);
    }

    #[test]
    fn older_pages_deduplicate_without_regressing_edits_or_tombstones() {
        let mut cache = MessageCache::default();
        let mut deleted = message(3);
        deleted.revision = 3;
        deleted.deleted = true;
        deleted.content.clear();
        merge(
            &mut cache,
            vec![message(4), deleted, message(2)],
            Merge::Older,
        );
        merge(
            &mut cache,
            vec![message(3), message(1), message(2)],
            Merge::Older,
        );
        assert_eq!(
            cache.order.keys().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        let row = cache.get(message(3).id).unwrap();
        assert!(row.deleted);
        assert!(row.content.is_empty());
        assert_eq!(row.revision, 3);
    }

    #[test]
    fn paging_back_keeps_older_window_and_requires_latest_snapshot() {
        let mut cache = MessageCache::default();
        merge(
            &mut cache,
            (501..=1000).map(message).collect(),
            Merge::Older,
        );
        merge(&mut cache, (451..=500).map(message).collect(), Merge::Older);
        assert_eq!(
            cache.order.first_key_value().map(|(seq, _)| *seq),
            Some(451)
        );
        assert_eq!(cache.latest_sequence(), 950);
        assert!(cache.newer);
        // Live sends cannot append across the evicted gap, even at the window's bottom.
        merge(
            &mut cache,
            vec![message(1001)],
            Merge::Live { follow: true },
        );
        assert_eq!(cache.latest_sequence(), 950);
        // Existing rows still receive edits while detached.
        let mut edit = message(600);
        edit.revision = 2;
        edit.content = "updated".into();
        merge(&mut cache, vec![edit], Merge::Live { follow: false });
        assert_eq!(cache.get(message(600).id).unwrap().content, "updated");
    }

    #[test]
    fn live_traffic_does_not_evict_the_window_being_read() {
        let mut cache = MessageCache::default();
        merge(&mut cache, (1..=500).map(message).collect(), Merge::Older);
        for sequence in 501..=1500 {
            merge(
                &mut cache,
                vec![message(sequence)],
                Merge::Live { follow: false },
            );
        }
        // The rows being read stay resident; newer traffic waits outside.
        assert_eq!(
            cache.ordered().next().map(|(id, _)| id),
            Some(message(1).id)
        );
        assert_eq!(cache.latest_sequence(), 500);
        assert!(cache.newer);
    }

    #[test]
    fn eviction_drops_row_state() {
        struct Row(Rc<Cell<usize>>);
        impl Drop for Row {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let dropped = Rc::new(Cell::new(0));
        let mut cache = MessageCache::default();
        for sequence in 1..=2000 {
            cache.merge(
                [message(sequence)],
                Merge::Live { follow: true },
                |_| Row(dropped.clone()),
                |_, _| {},
            );
        }
        assert_eq!(dropped.get(), 1500);
        drop(cache);
        assert_eq!(dropped.get(), 2000);
    }

    #[test]
    fn previous_and_latest_follow_channel_order_not_arrival() {
        let mut cache = MessageCache::default();
        merge(&mut cache, vec![message(5), message(3)], Merge::Older);
        merge(&mut cache, vec![message(4)], Merge::Older);
        assert_eq!(
            cache.previous(message(5).id).map(|(id, _)| id),
            Some(message(4).id)
        );
        assert_eq!(
            cache.previous(message(4).id).map(|(id, _)| id),
            Some(message(3).id)
        );
        assert!(cache.previous(message(3).id).is_none());
        assert!(cache.previous(message(9).id).is_none());
        let order: Vec<_> = cache.ordered().map(|(id, _)| id).collect();
        assert_eq!(order, [3, 4, 5].map(|s| message(s).id));
        assert_eq!(cache.latest_where(|m| m.sequence < 5), Some(message(4).id));
        assert_eq!(cache.latest_where(|_| false), None);
    }

    #[test]
    fn grouping_requires_same_known_author_in_order_within_window() {
        let author = Some("00000000-0000-0000-0000-000000000001".parse().unwrap());
        let other = Some("00000000-0000-0000-0000-000000000002".parse().unwrap());
        let at = Timestamp::from_timestamp(1_000_000, 0).unwrap();
        let later = |seconds: i64| Timestamp::from_timestamp(1_000_000 + seconds, 0).unwrap();
        assert!(groups_with(author, at, author, later(0)));
        assert!(groups_with(
            author,
            at,
            author,
            later(GROUP_WINDOW_SECONDS - 1)
        ));
        assert!(!groups_with(
            author,
            at,
            author,
            later(GROUP_WINDOW_SECONDS)
        ));
        assert!(!groups_with(author, at, author, later(-1)));
        assert!(!groups_with(author, at, other, later(1)));
        // Deleted accounts have no author and never merge.
        assert!(!groups_with(None, at, None, later(1)));
    }
}
