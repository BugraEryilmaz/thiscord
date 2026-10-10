//! Bounded, ID-indexed history and variable-height window geometry, independent of the DOM.
use std::collections::{BTreeMap, HashMap};
use thiscord_shared::{MessageId, chat::ChatMessage, pagination::PageCursor};

pub const RESIDENT_LIMIT: usize = 500;
pub const ESTIMATED_HEIGHT: f64 = 112.0;
pub const MIN_ROW_HEIGHT: f64 = 80.0;
const OVERSCAN: f64 = 400.0;

struct Entry<T> {
    value: T,
    revision: i32,
    cursor: PageCursor,
    height: f64,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    id: MessageId,
    offset: f64,
}

#[derive(Clone, Default, PartialEq)]
pub struct Window {
    pub ids: Vec<MessageId>,
    pub before: f64,
    pub after: f64,
}

impl<T> MessageCache<T> {
    pub fn get(&self, id: MessageId) -> Option<&T> {
        self.by_id.get(&id).map(|entry| &entry.value)
    }

    pub fn latest_sequence(&self) -> i64 {
        self.order.last_key_value().map_or(0, |(seq, _)| *seq)
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
                    cursor: PageCursor::new(message.created_at, message.id.as_uuid()),
                    height: ESTIMATED_HEIGHT,
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

    pub fn measure(&mut self, id: MessageId, height: f64) -> bool {
        if !height.is_finite() {
            return false;
        }
        let Some(entry) = self.by_id.get_mut(&id) else {
            return false;
        };
        let height = height.max(MIN_ROW_HEIGHT);
        if (entry.height - height).abs() < 0.5 {
            return false;
        }
        entry.height = height;
        true
    }

    pub fn anchor(&self, top: f64) -> Option<Anchor> {
        let mut y = 0.0;
        for id in self.order.values() {
            let height = self.by_id[id].height;
            if y + height > top {
                return Some(Anchor {
                    id: *id,
                    offset: top - y,
                });
            }
            y += height;
        }
        None
    }

    pub fn anchor_top(&self, anchor: Anchor) -> Option<f64> {
        let mut y = 0.0;
        for id in self.order.values() {
            if *id == anchor.id {
                return Some(y + anchor.offset);
            }
            y += self.by_id[id].height;
        }
        None
    }

    pub fn window(&self, top: f64, viewport: f64) -> Window {
        let mut result = Window::default();
        let mut y = 0.0;
        let start = (top - OVERSCAN).max(0.0);
        let end = top.max(0.0) + viewport.max(0.0) + OVERSCAN;
        for id in self.order.values() {
            let height = self.by_id[id].height;
            if y + height <= start {
                result.before += height;
            } else if y < end {
                result.ids.push(*id);
            } else {
                result.after += height;
            }
            y += height;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use thiscord_shared::Timestamp;

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
            content: format!("message {sequence}"),
            mentions: vec![],
            created_at: Timestamp::from_timestamp(sequence, 0).unwrap(),
            edited_at: None,
            deleted: false,
            revision: 1,
            sequence,
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
        let anchor = cache.anchor(100.0).unwrap();
        merge(&mut cache, (451..=500).map(message).collect(), Merge::Older);
        assert_eq!(
            cache.order.first_key_value().map(|(seq, _)| *seq),
            Some(451)
        );
        assert_eq!(cache.latest_sequence(), 950);
        assert!(cache.newer);
        assert_eq!(
            cache.anchor_top(anchor),
            Some(100.0 + 50.0 * ESTIMATED_HEIGHT)
        );
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
        let anchor = cache.anchor(600.0).unwrap();
        for sequence in 501..=1500 {
            merge(
                &mut cache,
                vec![message(sequence)],
                Merge::Live { follow: false },
            );
        }
        assert_eq!(cache.anchor_top(anchor), Some(600.0));
        assert_eq!(cache.latest_sequence(), 500);
        assert!(cache.newer);
    }

    #[test]
    fn measured_window_covers_viewport_and_preserves_anchor_on_resize() {
        let mut cache = MessageCache::default();
        merge(&mut cache, (1..=500).map(message).collect(), Merge::Older);
        let top = 50.0 * ESTIMATED_HEIGHT + 20.0;
        let anchor = cache.anchor(top).unwrap();
        assert!(cache.measure(message(49).id, 3000.0));
        assert!(!cache.measure(message(49).id, 3000.0));
        assert_eq!(
            cache.anchor_top(anchor),
            Some(top + 3000.0 - ESTIMATED_HEIGHT)
        );
        for height in [300.0, 800.0, 2000.0] {
            let view = cache.window(top, height);
            assert!(
                view.ids.len() <= ((height + 2.0 * OVERSCAN) / MIN_ROW_HEIGHT).ceil() as usize + 2
            );
            assert!(view.before <= top);
            let rendered: f64 = view.ids.iter().map(|id| cache.by_id[id].height).sum();
            assert!(view.before + rendered >= top + height);
            assert_eq!(
                view.before + rendered + view.after,
                499.0 * ESTIMATED_HEIGHT + 3000.0
            );
        }
        // Shrinking the long row (e.g. deletion) restores the original anchor offset.
        cache.measure(message(49).id, ESTIMATED_HEIGHT);
        assert_eq!(cache.anchor_top(anchor), Some(top));
    }

    #[test]
    fn eviction_drops_row_state_and_measurements() {
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
            cache.measure(message(sequence).id, 200.0);
        }
        assert_eq!(dropped.get(), 1500);
        drop(cache);
        assert_eq!(dropped.get(), 2000);
    }

    #[test]
    fn prepend_measurements_keep_the_original_message_below_the_history_control() {
        let mut cache = MessageCache::default();
        merge(&mut cache, (101..=150).map(message).collect(), Merge::Older);
        // The first message begins below the history button and container padding.
        let anchor = cache.anchor(-56.0).unwrap();
        merge(&mut cache, (51..=100).map(message).collect(), Merge::Older);
        assert_eq!(
            cache.anchor_top(anchor),
            Some(50.0 * ESTIMATED_HEIGHT - 56.0)
        );
        for sequence in 95..=100 {
            cache.measure(message(sequence).id, 96.0);
        }
        assert_eq!(
            cache.anchor_top(anchor),
            Some(50.0 * ESTIMATED_HEIGHT - 56.0 - 6.0 * 16.0)
        );
        assert!(!cache.measure(message(101).id, f64::NAN));
        assert!(!cache.measure(message(101).id, f64::INFINITY));
    }
}
