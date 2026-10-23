//! Revision reconciliation shared by live rows, reply previews and HTTP snapshots.
//! All payloads belong to the current recipient; never reconstruct reaction `mine`.
use std::collections::{HashMap, VecDeque};
use thiscord_shared::{MessageId, chat::ChatMessage, chat::MAX_CHANNEL_PINS};

use crate::chat_history::RESIDENT_LIMIT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotToken {
    request: u64,
    since: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SnapshotStatus {
    Ready,
    Cancelled,
    /// Too many distinct updates to safely use the snapshot. Fetch it again.
    Overflow,
}

#[derive(Default)]
pub struct RichChat {
    // Retain updates even when their target is outside the resident window.
    recent: HashMap<MessageId, (u64, ChatMessage)>,
    order: VecDeque<MessageId>,
    clock: u64,
    evicted_through: u64,
    request: u64,
    context: Option<SnapshotToken>,
    pins_request: Option<SnapshotToken>,
    pins_open: bool,
    pins: Vec<ChatMessage>,
}

impl RichChat {
    pub fn reset(&mut self) {
        // Never reuse a token after reconnect/access reset.
        let request = self.request + 1;
        *self = Self {
            request,
            ..Self::default()
        };
    }

    fn token(&mut self) -> SnapshotToken {
        self.request += 1;
        SnapshotToken {
            request: self.request,
            since: self.clock,
        }
    }

    pub fn begin_context(&mut self) -> SnapshotToken {
        let token = self.token();
        self.context = Some(token);
        token
    }

    pub fn cancel_context(&mut self) {
        self.context = None;
    }

    fn finish(&self, pending: Option<SnapshotToken>, token: SnapshotToken) -> SnapshotStatus {
        if pending != Some(token) {
            SnapshotStatus::Cancelled
        } else if self.evicted_through > token.since {
            SnapshotStatus::Overflow
        } else {
            SnapshotStatus::Ready
        }
    }

    pub fn finish_context(&mut self, token: SnapshotToken) -> SnapshotStatus {
        let status = self.finish(self.context, token);
        if status != SnapshotStatus::Cancelled {
            self.context = None;
        }
        status
    }

    pub fn begin_pins(&mut self) -> SnapshotToken {
        let token = self.token();
        self.pins_request = Some(token);
        self.pins_open = false;
        self.pins.clear();
        token
    }

    pub fn close_pins(&mut self) {
        self.pins_request = None;
        self.pins_open = false;
        self.pins.clear();
    }

    pub fn cancel_pins(&mut self, token: SnapshotToken) -> bool {
        if self.pins_request != Some(token) {
            return false;
        }
        self.close_pins();
        true
    }

    pub fn pins_open(&self) -> bool {
        self.pins_open
    }
    pub fn pins_loading(&self) -> bool {
        self.pins_request.is_some()
    }
    pub fn pins(&self) -> &[ChatMessage] {
        &self.pins
    }

    pub fn finish_pins(
        &mut self,
        token: SnapshotToken,
        mut messages: Vec<ChatMessage>,
        resident: impl Fn(MessageId) -> Option<ChatMessage>,
    ) -> SnapshotStatus {
        let status = self.finish(self.pins_request, token);
        if status == SnapshotStatus::Cancelled {
            return status;
        }
        self.pins_request = None;
        if status == SnapshotStatus::Ready {
            // Include newly pinned messages absent from the HTTP snapshot, plus
            // unpins/deletes which must suppress rows present in that snapshot.
            messages.extend(
                self.recent
                    .values()
                    .filter(|(clock, _)| *clock > token.since)
                    .map(|(_, message)| message.clone()),
            );
            let messages = self.reconcile(messages, resident);
            self.pins = messages
                .into_iter()
                .filter(|m| m.pinned && !m.deleted)
                .collect();
            self.pins.sort_by_key(|m| std::cmp::Reverse(m.sequence));
            self.pins.truncate(MAX_CHANNEL_PINS);
            self.pins_open = true;
        }
        status
    }

    /// Resolve the entire batch before exposing any payload to UI side effects.
    /// A stale response is replaced with the latest recipient-specific payload.
    pub fn reconcile(
        &mut self,
        messages: Vec<ChatMessage>,
        resident: impl Fn(MessageId) -> Option<ChatMessage>,
    ) -> Vec<ChatMessage> {
        let mut batch = HashMap::<MessageId, ChatMessage>::new();
        for mut message in messages {
            for known in [
                self.recent.get(&message.id).map(|(_, m)| m.clone()),
                resident(message.id),
                self.pins.iter().find(|m| m.id == message.id).cloned(),
                batch.get(&message.id).cloned(),
            ]
            .into_iter()
            .flatten()
            {
                if known.revision >= message.revision {
                    message = known;
                }
            }
            batch.insert(message.id, message);
        }
        // Reply previews have no revision of their own. Even a newer reaction
        // response may embed an old preview of an independently deleted target.
        let previews: HashMap<_, _> = batch
            .values()
            .filter_map(|m| m.reply.as_ref())
            .filter_map(|reply| {
                let target = batch
                    .get(&reply.id)
                    .cloned()
                    .or_else(|| self.recent.get(&reply.id).map(|(_, m)| m.clone()));
                let current = resident(reply.id);
                let target = match (target, current) {
                    (Some(a), Some(b)) => Some(if b.revision > a.revision { b } else { a }),
                    (a, b) => a.or(b),
                }?;
                Some((reply.id, preview(&target)))
            })
            .collect();
        let mut messages: Vec<_> = batch.into_values().collect();
        messages.sort_by_key(|m| m.sequence);
        for message in &mut messages {
            if let Some(reply) = &mut message.reply
                && let Some(preview) = previews.get(&reply.id)
            {
                reply.preview = preview.clone();
            }
            self.remember(message);
        }
        if self.pins_open {
            // Remove first so an unpin and a new pin in one batch work at the cap.
            self.pins
                .retain(|pin| !messages.iter().any(|m| m.id == pin.id));
            self.pins
                .extend(messages.iter().filter(|m| m.pinned && !m.deleted).cloned());
            self.pins.sort_by_key(|m| std::cmp::Reverse(m.sequence));
            self.pins.truncate(MAX_CHANNEL_PINS);
        }
        messages
    }

    fn remember(&mut self, message: &ChatMessage) {
        if let Some((_, known)) = self.recent.get_mut(&message.id)
            && known.revision >= message.revision
        {
            // Keep normalized previews without recording a duplicate as an event.
            known.reply = message.reply.clone();
            return;
        }
        self.clock += 1;
        self.order.retain(|id| *id != message.id);
        self.order.push_back(message.id);
        self.recent
            .insert(message.id, (self.clock, message.clone()));
        if self.order.len() > RESIDENT_LIMIT {
            let id = self.order.pop_front().expect("nonempty updates");
            let (clock, _) = self.recent.remove(&id).expect("indexed update");
            self.evicted_through = self.evicted_through.max(clock);
        }
    }
}

pub fn preview(message: &ChatMessage) -> Option<String> {
    (!message.deleted).then(|| message.content.chars().take(160).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_history::{Merge, MessageCache};
    use thiscord_shared::{
        Timestamp,
        chat::{ChatAttachment, Reaction, ReplyPreview},
    };

    fn message(sequence: i64, revision: i32) -> ChatMessage {
        let id = format!("00000000-0000-0000-0000-{sequence:012x}");
        ChatMessage {
            id: id.parse().unwrap(),
            client_id: id.parse().unwrap(),
            guild_id: id.parse().unwrap(),
            channel_id: id.parse().unwrap(),
            author_id: None,
            username: "test".into(),
            display_name: String::new(),
            content: format!("message {sequence} revision {revision}"),
            attachments: vec![],
            mentions: vec![],
            created_at: Timestamp::from_timestamp(sequence, 0).unwrap(),
            edited_at: None,
            deleted: false,
            revision,
            sequence,
            reply: None,
            reactions: vec![],
            pinned: false,
        }
    }

    fn deleted(sequence: i64, revision: i32) -> ChatMessage {
        let mut message = message(sequence, revision);
        message.deleted = true;
        message.content.clear();
        message
    }

    fn pinned(sequence: i64, revision: i32) -> ChatMessage {
        let mut message = message(sequence, revision);
        message.pinned = true;
        message
    }

    fn reaction(message: &mut ChatMessage, mine: bool) {
        message.reactions = vec![Reaction {
            emoji: "👍".into(),
            count: 2,
            mine,
        }];
    }

    fn cache_merge(cache: &mut MessageCache<ChatMessage>, messages: Vec<ChatMessage>, mode: Merge) {
        cache.merge(messages, mode, |m| m, |old, m| *old = m);
    }

    #[test]
    fn delayed_edit_and_reaction_cannot_restore_deleted_preview_pin_or_attachment() {
        let mut state = RichChat::default();
        let mut cache = MessageCache::default();
        let mut old = pinned(1, 2);
        old.attachments.push(ChatAttachment {
            id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            filename: "private.txt".into(),
            content_type: "text/plain".into(),
            size_bytes: 10,
        });
        let ticket = state.begin_pins();
        assert_eq!(
            state.finish_pins(ticket, vec![old.clone()], |_| None),
            SnapshotStatus::Ready
        );
        let tombstone = deleted(1, 4);
        let updates = state.reconcile(vec![tombstone], |_| None);
        cache_merge(&mut cache, updates, Merge::Older);
        // The deleted original is evicted, while its dependent reply can remain.
        cache_merge(
            &mut cache,
            (2..=501).map(|id| message(id, 1)).collect(),
            Merge::Live { follow: true },
        );
        assert!(cache.get(old.id).is_none());
        let mut delayed_reaction = old.clone();
        delayed_reaction.revision = 3;
        reaction(&mut delayed_reaction, true);
        for delayed in [old, delayed_reaction] {
            let delivered = state.reconcile(vec![delayed], |id| cache.get(id).cloned());
            let accepted = &delivered[0];
            assert!(accepted.deleted);
            assert_eq!(accepted.revision, 4);
            assert_eq!(preview(accepted), None);
            assert!(accepted.content.is_empty() && accepted.attachments.is_empty());
            assert!(!accepted.pinned);
            assert!(state.pins().is_empty());
        }
    }

    #[test]
    fn reaction_response_for_reply_cannot_restore_its_independently_deleted_target() {
        let mut state = RichChat::default();
        state.reconcile(vec![deleted(1, 3)], |_| None);
        let mut reply = message(2, 8);
        reply.reply = Some(ReplyPreview {
            id: message(1, 1).id,
            preview: Some("old private text".into()),
        });
        reaction(&mut reply, true);
        let delivered = state.reconcile(vec![reply], |_| None);
        assert_eq!(delivered[0].reply.as_ref().unwrap().preview, None);
        assert!(delivered[0].reactions[0].mine);
    }

    #[test]
    fn context_keeps_intervening_edits_deletes_and_recipient_reactions_outside_old_window() {
        let mut state = RichChat::default();
        let mut cache = MessageCache::default();
        cache_merge(&mut cache, vec![message(100, 1)], Merge::Older);
        let ticket = state.begin_context();
        let mut reacted = message(3, 5);
        reaction(&mut reacted, true);
        for update in [message(1, 2), deleted(2, 3), reacted, message(101, 1)] {
            let delivered = state.reconcile(vec![update], |id| cache.get(id).cloned());
            cache_merge(&mut cache, delivered, Merge::Live { follow: false });
        }
        assert!(cache.get(message(1, 1).id).is_none());
        assert_eq!(state.finish_context(ticket), SnapshotStatus::Ready);
        let snapshot = state.reconcile((1..=3).map(|id| message(id, 1)).collect(), |id| {
            cache.get(id).cloned()
        });
        cache = MessageCache::default();
        cache.newer = true;
        cache_merge(&mut cache, snapshot, Merge::Older);
        assert_eq!(
            cache.get(message(1, 1).id).unwrap().content,
            "message 1 revision 2"
        );
        assert!(cache.get(message(2, 1).id).unwrap().deleted);
        assert!(cache.get(message(3, 1).id).unwrap().reactions[0].mine);
        assert!(cache.newer);
        assert_eq!(cache.latest_sequence(), 3);
        assert!(cache.get(message(101, 1).id).is_none());
    }

    #[test]
    fn context_uses_newer_resident_revision_even_after_recent_record_expires() {
        let mut state = RichChat::default();
        let edited = message(1, 7);
        state.reconcile(vec![edited.clone()], |_| None);
        for id in 2..=502 {
            state.reconcile(vec![message(id, 1)], |_| None);
        }
        let ticket = state.begin_context();
        assert_eq!(state.finish_context(ticket), SnapshotStatus::Ready);
        let snapshot = state.reconcile(vec![message(1, 1)], |id| {
            (id == edited.id).then(|| edited.clone())
        });
        assert_eq!(snapshot[0].revision, 7);
    }

    #[test]
    fn cached_jump_cancels_context_a_and_cannot_cancel_a_later_context_c() {
        let mut state = RichChat::default();
        let a = state.begin_context();
        state.cancel_context(); // Jump B is already resident; no HTTP request.
        assert_eq!(state.finish_context(a), SnapshotStatus::Cancelled);
        let c = state.begin_context();
        assert_eq!(state.finish_context(a), SnapshotStatus::Cancelled);
        assert_eq!(state.finish_context(c), SnapshotStatus::Ready);
    }

    #[test]
    fn pins_loading_reconciles_pin_unpin_delete_and_both_mine_values() {
        for mine in [true, false] {
            let mut state = RichChat::default();
            let ticket = state.begin_pins();
            assert!(!state.pins_open() && state.pins_loading());
            let mut newly_pinned = pinned(4, 2);
            reaction(&mut newly_pinned, mine);
            let mut reacted = pinned(3, 3);
            reaction(&mut reacted, mine);
            // Separate deliveries before any UI effects/render can run.
            for update in [message(1, 2), deleted(2, 3), reacted, newly_pinned] {
                state.reconcile(vec![update], |_| None);
            }
            let snapshot = (1..=3).map(|id| pinned(id, 1)).collect();
            assert_eq!(
                state.finish_pins(ticket, snapshot, |_| None),
                SnapshotStatus::Ready
            );
            assert!(state.pins_open() && !state.pins_loading());
            assert_eq!(
                state.pins().iter().map(|m| m.sequence).collect::<Vec<_>>(),
                vec![4, 3]
            );
            assert!(state.pins().iter().all(|m| m.reactions[0].mine == mine));
            // A late action response cannot re-add an unpinned/deleted message.
            state.reconcile(vec![pinned(1, 1), pinned(2, 1)], |_| None);
            assert_eq!(state.pins().len(), 2);
        }
    }

    #[test]
    fn open_pins_keep_revision_and_replace_a_pin_in_a_full_batch() {
        let mut state = RichChat::default();
        let ticket = state.begin_pins();
        let pins = (1..=MAX_CHANNEL_PINS as i64)
            .map(|id| pinned(id, 2))
            .collect();
        state.finish_pins(ticket, pins, |_| None);
        // Expire the recent record; the open pin remains an authoritative copy.
        for id in 100..=700 {
            state.reconcile(vec![message(id, 1)], |_| None);
        }
        state.reconcile(vec![message(1, 1)], |_| None);
        assert!(state.pins().iter().any(|m| m.sequence == 1));
        state.reconcile(vec![pinned(51, 1), message(1, 3)], |_| None);
        assert_eq!(state.pins().len(), MAX_CHANNEL_PINS);
        assert!(state.pins().iter().any(|m| m.sequence == 51));
        assert!(!state.pins().iter().any(|m| m.sequence == 1));
    }

    #[test]
    fn overflow_never_publishes_a_snapshot_with_lost_updates_and_retry_is_usable() {
        let mut state = RichChat::default();
        let context = state.begin_context();
        let pins = state.begin_pins();
        for id in 1..=RESIDENT_LIMIT as i64 + 1 {
            state.reconcile(vec![deleted(id, 2)], |_| None);
        }
        assert_eq!(state.finish_context(context), SnapshotStatus::Overflow);
        assert_eq!(
            state.finish_pins(pins, vec![pinned(1, 1)], |_| None),
            SnapshotStatus::Overflow
        );
        assert!(!state.pins_open());
        let retry = state.begin_pins();
        assert_eq!(
            state.finish_pins(retry, vec![], |_| None),
            SnapshotStatus::Ready
        );
        assert!(state.pins().is_empty());
        assert_eq!(state.recent.len(), RESIDENT_LIMIT);
        assert_eq!(state.order.len(), RESIDENT_LIMIT);
    }

    #[test]
    fn repeated_updates_coalesce_without_overflow_and_reset_rejects_old_requests() {
        let mut state = RichChat::default();
        let context = state.begin_context();
        let pins = state.begin_pins();
        for revision in 1..=2000 {
            state.reconcile(vec![pinned(1, revision)], |_| None);
        }
        assert_eq!(state.finish_context(context), SnapshotStatus::Ready);
        assert_eq!(
            state.finish_pins(pins, vec![pinned(1, 1)], |_| None),
            SnapshotStatus::Ready
        );
        assert_eq!(state.pins()[0].revision, 2000);
        let old_context = state.begin_context();
        let old_pins = state.begin_pins();
        state.reset();
        assert!(state.recent.is_empty() && state.pins().is_empty());
        let new_context = state.begin_context();
        let new_pins = state.begin_pins();
        // A failed pre-reset request must not close a later request's panel.
        assert!(!state.cancel_pins(old_pins));
        assert!(state.pins_loading());
        assert_eq!(state.finish_context(old_context), SnapshotStatus::Cancelled);
        assert_eq!(
            state.finish_pins(old_pins, vec![pinned(1, 2000)], |_| None),
            SnapshotStatus::Cancelled
        );
        assert_eq!(state.finish_context(new_context), SnapshotStatus::Ready);
        assert_eq!(
            state.finish_pins(new_pins, vec![], |_| None),
            SnapshotStatus::Ready
        );
        assert!(state.pins().is_empty());
    }
}
