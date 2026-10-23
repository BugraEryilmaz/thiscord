//! Revision reconciliation shared by live rows, reply previews and HTTP snapshots.
//! All payloads belong to the current recipient; never reconstruct reaction `mine`.
use std::collections::{HashMap, VecDeque};
use thiscord_shared::{
    MessageId,
    chat::{ChatMessage, MAX_CHANNEL_PINS, ReplyPreview},
};

use crate::chat_history::RESIDENT_LIMIT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotToken {
    request: u64,
    since: u64,
    generation: u64,
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
    generation: u64,
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
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    fn token(&mut self) -> SnapshotToken {
        self.request += 1;
        SnapshotToken {
            request: self.request,
            since: self.clock,
            generation: self.generation,
        }
    }

    /// Every HTTP response that can merge messages needs an eviction watermark,
    /// including actions and older history. Tokens live with their requests, so
    /// tracking concurrent requests does not grow the reconciliation cache.
    pub fn begin_response(&mut self) -> SnapshotToken {
        self.token()
    }

    pub fn response_status(&self, token: SnapshotToken) -> SnapshotStatus {
        if token.generation != self.generation {
            SnapshotStatus::Cancelled
        } else if self.evicted_through > token.since {
            SnapshotStatus::Overflow
        } else {
            SnapshotStatus::Ready
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
        } else {
            self.response_status(token)
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
            let updates: Vec<_> = self
                .recent
                .values()
                .filter(|(clock, message)| {
                    *clock > token.since && !messages.iter().any(|m| m.id == message.id)
                })
                .map(|(_, message)| message.clone())
                .collect();
            // Reconcile compares both message and target revisions with retained
            // state, so each snapshot row needs only one input copy.
            messages.extend(updates);
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
                // Body and embedded target were read at independent revisions.
                // Resolve the preview before selecting the newer reply body.
                let mut reply = known.reply.clone();
                if let (Some(retained), Some(incoming)) = (&mut reply, &message.reply) {
                    reconcile_reply(retained, incoming);
                }
                if known.revision > message.revision {
                    message = known;
                }
                if let (Some(current), Some(reply)) = (&mut message.reply, reply)
                    && current.id == reply.id
                {
                    *current = reply;
                }
            }
            batch.insert(message.id, message);
        }
        // Compare target revisions from both full target rows and embedded
        // previews. A fresh HTTP preview may precede the target's socket event,
        // and the full target may never be resident. This temporary map is bounded
        // by the batch and existing bounded stores; it adds no persistent cache.
        let mut previews = HashMap::<MessageId, ReplyPreview>::new();
        for reply in self
            .recent
            .values()
            .map(|(_, m)| m)
            .chain(self.pins.iter())
            .chain(batch.values())
            .filter_map(|m| m.reply.as_ref())
        {
            let current = previews.entry(reply.id).or_insert_with(|| reply.clone());
            reconcile_reply(current, reply);
        }
        for message in self
            .recent
            .values()
            .map(|(_, m)| m)
            .chain(self.pins.iter())
            .chain(batch.values())
        {
            if let Some(current) = previews.get_mut(&message.id) {
                reconcile_reply(current, &reply_preview(message));
            }
        }
        for (id, current) in &mut previews {
            if let Some(target) = resident(*id) {
                reconcile_reply(current, &reply_preview(&target));
            }
        }
        let mut messages: Vec<_> = batch.into_values().collect();
        messages.sort_by_key(|m| m.sequence);
        for message in &mut messages {
            if let Some(reply) = &mut message.reply
                && let Some(preview) = previews.get(&reply.id)
            {
                reconcile_reply(reply, preview);
            }
            self.remember(message);
        }
        // Target edits/deletes must update every retained reply, even when the
        // reply is outside the resident window. Otherwise a pin or recent copy
        // could restore its preview after the target's bounded record expires.
        let updated_replies: Vec<_> = self
            .recent
            .values()
            .map(|(_, m)| m)
            .chain(self.pins.iter())
            .filter_map(|retained| {
                let reply = retained.reply.as_ref()?;
                let preview = previews.get(&reply.id)?;
                let mut updated = retained.clone();
                reconcile_reply(updated.reply.as_mut().expect("retained reply"), preview)
                    .then_some(updated)
            })
            .collect();
        for updated in updated_replies {
            // Propagation is an update too, including to pins whose recent
            // record expired. Retain it before the pins panel can be closed.
            self.remember(&updated);
            if let Some(pin) = self.pins.iter_mut().find(|m| m.id == updated.id) {
                pin.reply = updated.reply;
            }
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
        let message = if let Some((_, known)) = self.recent.get(&message.id)
            && known.revision >= message.revision
        {
            if known.reply == message.reply {
                return;
            }
            // A target preview can change without the reply's revision moving.
            // Refresh its clock and eviction position just like a body update,
            // preserving the retained recipient-specific message payload.
            let mut updated = known.clone();
            updated.reply = message.reply.clone();
            updated
        } else {
            message.clone()
        };
        self.clock += 1;
        self.order.retain(|id| *id != message.id);
        self.order.push_back(message.id);
        self.recent.insert(message.id, (self.clock, message));
        if self.order.len() > RESIDENT_LIMIT {
            let id = self.order.pop_front().expect("nonempty updates");
            let (clock, _) = self.recent.remove(&id).expect("indexed update");
            self.evicted_through = self.evicted_through.max(clock);
        }
    }
}

/// Merge only independently versioned target data, never the reply body or `mine`.
/// Missing legacy revisions are unknown (older than any known revision); ties
/// retain existing content. Unavailable previews remain unavailable until reset.
pub fn reconcile_reply(current: &mut ReplyPreview, incoming: &ReplyPreview) -> bool {
    if current.id != incoming.id {
        return false;
    }
    let replace = if current.preview.is_none() {
        incoming.preview.is_none() && incoming.target_revision > current.target_revision
    } else {
        incoming.preview.is_none() || incoming.target_revision > current.target_revision
    };
    if replace {
        *current = incoming.clone();
    }
    replace
}

pub fn reply_preview(message: &ChatMessage) -> ReplyPreview {
    ReplyPreview {
        id: message.id,
        preview: preview(message),
        target_revision: Some(message.revision),
    }
}

/// Update a resident UI row even when its body revision did not advance.
/// Use the same comparison for full target events and embedded HTTP previews.
pub fn updated_reply(message: &ChatMessage, updates: &[ChatMessage]) -> Option<ReplyPreview> {
    let current = message.reply.as_ref()?;
    let mut reply = current.clone();
    for update in updates {
        if update.id == reply.id {
            reconcile_reply(&mut reply, &reply_preview(update));
        }
        if let Some(incoming) = &update.reply {
            reconcile_reply(&mut reply, incoming);
        }
    }
    (reply != *current).then_some(reply)
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
    fn full_target_events_and_ui_rows_preserve_newer_embedded_previews() {
        let old_target = message(1, 1);
        let fresh_target = message(1, 4);
        let mut reply = pinned(2, 20);
        reply.reply = Some(reply_preview(&fresh_target));
        let mut state = RichChat::default();
        let pins = state.begin_pins();
        state.finish_pins(pins, vec![reply.clone()], |_| None);
        let context = state.begin_context();
        assert_eq!(state.finish_context(context), SnapshotStatus::Ready);
        let delivered = state.reconcile(vec![old_target.clone(), reply.clone()], |_| None);
        assert!(delivered[1].reply == reply.reply);
        assert!(state.pins()[0].reply == reply.reply);
        // This helper is used by the WASM caller after MessageCache::merge.
        assert!(updated_reply(&reply, std::slice::from_ref(&old_target)).is_none());
        let newer = message(1, 5);
        reply.reply = updated_reply(&reply, std::slice::from_ref(&newer));
        assert!(reply.reply == Some(reply_preview(&newer)));
        let delivered = state.reconcile(vec![newer], |_| None);
        assert!(updated_reply(&reply, &delivered).is_none());
        assert!(state.pins()[0].reply == reply.reply);
        // A later HTTP preview can also update another reply to the same target.
        let mut second_reply = message(3, 1);
        second_reply.reply = Some(reply_preview(&old_target));
        second_reply.reply = updated_reply(&second_reply, std::slice::from_ref(&reply));
        assert!(second_reply.reply == reply.reply);
    }

    #[test]
    fn legacy_preview_does_not_override_known_target_revision_or_unavailability() {
        let mut legacy = ReplyPreview {
            id: message(1, 1).id,
            preview: Some("legacy".into()),
            target_revision: None,
        };
        let known = reply_preview(&message(1, 0));
        assert!(reconcile_reply(&mut legacy, &known));
        let mut unknown = legacy.clone();
        unknown.target_revision = None;
        unknown.preview = Some("unknown freshness".into());
        assert!(!reconcile_reply(&mut legacy, &unknown));
        unknown.preview = None;
        assert!(reconcile_reply(&mut legacy, &unknown));
        assert!(!reconcile_reply(&mut legacy, &known));
        assert_eq!(legacy.preview, None);
    }

    #[test]
    fn edited_pins_preview_survives_close_and_delayed_older_history() {
        for fresh_first in [false, true] {
            let mut state = RichChat::default();
            let mut old = pinned(2, 7);
            old.reply = Some(ReplyPreview {
                id: message(1, 1).id,
                preview: Some("old target text".into()),
                target_revision: Some(1),
            });
            reaction(&mut old, true);
            let mut fresh = old.clone();
            fresh.reply.as_mut().unwrap().preview = Some("edited target text".into());
            fresh.reply.as_mut().unwrap().target_revision = Some(2);
            state.reconcile(vec![old.clone()], |_| None);
            let history = state.begin_response();
            let pins = state.begin_pins();
            if !fresh_first {
                assert_eq!(state.response_status(history), SnapshotStatus::Ready);
                state.reconcile(vec![old.clone()], |_| None);
            }
            assert_eq!(
                state.finish_pins(pins, vec![fresh.clone()], |_| None),
                SnapshotStatus::Ready
            );
            state.close_pins();
            assert!(!state.recent.contains_key(&message(1, 1).id));
            let clock = state.clock;
            if fresh_first {
                assert_eq!(state.response_status(history), SnapshotStatus::Ready);
                let delivered = state.reconcile(vec![old.clone()], |_| None);
                assert!(delivered[0].reply == fresh.reply);
                assert_eq!(state.clock, clock);
            }
            assert!(state.recent[&old.id].1.reply == fresh.reply);
            assert!(state.recent[&old.id].1.reactions[0].mine);
            assert!(clock > history.since);
            // Only eviction of the preview-bearing reply loses the evidence.
            // The in-flight older snapshot must then be rejected, not merged.
            for id in 3..=502 {
                state.reconcile(vec![message(id, 1)], |_| None);
            }
            assert_eq!(state.response_status(history), SnapshotStatus::Overflow);
            assert_eq!(state.recent.len(), RESIDENT_LIMIT);
            assert_eq!(state.order.len(), RESIDENT_LIMIT);
        }
    }

    #[test]
    fn reply_body_and_target_preview_choose_revisions_independently() {
        for fresh_first in [false, true] {
            for mine in [false, true] {
                let mut state = RichChat::default();
                let mut body = message(2, 20);
                body.reply = Some(ReplyPreview {
                    id: message(1, 1).id,
                    preview: Some("old".into()),
                    target_revision: Some(1),
                });
                reaction(&mut body, mine);
                let mut fresh_preview = message(2, 3);
                fresh_preview.reply = Some(ReplyPreview {
                    id: message(1, 1).id,
                    preview: Some("edited".into()),
                    target_revision: Some(4),
                });
                reaction(&mut fresh_preview, !mine);
                let ordered = if fresh_first {
                    [fresh_preview.clone(), body.clone()]
                } else {
                    [body.clone(), fresh_preview.clone()]
                };
                state.reconcile(vec![ordered[0].clone()], |_| None);
                let result = state.reconcile(vec![ordered[1].clone()], |_| None);
                assert_eq!(result[0].revision, body.revision);
                assert_eq!(result[0].content, body.content);
                assert_eq!(result[0].reactions[0].mine, mine);
                assert!(result[0].reply == fresh_preview.reply);
            }
        }
    }

    #[test]
    fn pins_preview_change_survives_close_and_guards_delayed_history_after_eviction() {
        let mut state = RichChat::default();
        let mut reply = pinned(2, 3);
        reply.reply = Some(ReplyPreview {
            target_revision: Some(1),
            id: message(1, 1).id,
            preview: Some("old private text".into()),
        });
        let pins = state.begin_pins();
        assert_eq!(
            state.finish_pins(pins, vec![reply.clone()], |_| None),
            SnapshotStatus::Ready
        );
        for id in 3..=501 {
            state.reconcile(vec![message(id, 1)], |_| None);
        }
        assert_eq!(state.recent.len(), RESIDENT_LIMIT);
        assert_eq!(state.order.front(), Some(&reply.id));
        let history = state.begin_response();
        let later_history = state.begin_response();
        let pins = state.begin_pins();
        let mut fresh = reply.clone();
        fresh.reply.as_mut().unwrap().preview = None;
        // Only the HTTP read knows the target was deleted; no target event or
        // resident row can rescue the preview when the extra pin evicts a row.
        assert_eq!(
            state.finish_pins(pins, vec![fresh, pinned(502, 1)], |_| None),
            SnapshotStatus::Ready
        );
        state.close_pins();
        assert_eq!(state.response_status(history), SnapshotStatus::Ready);
        let clock = state.clock;
        let delivered = state.reconcile(vec![reply.clone()], |_| None);
        assert_eq!(delivered[0].reply.as_ref().unwrap().preview, None);
        assert_eq!(state.clock, clock);
        // A second pending copy must be rejected once the refreshed record is
        // evicted too. Stop before the new pin is evicted so that only the
        // preview change can advance the eviction watermark past the request.
        for id in 503..=1001 {
            state.reconcile(vec![message(id, 1)], |_| None);
        }
        assert!(!state.recent.contains_key(&reply.id));
        assert!(state.recent.contains_key(&message(502, 1).id));
        assert_eq!(
            state.response_status(later_history),
            SnapshotStatus::Overflow
        );
        assert_eq!(state.recent.len(), RESIDENT_LIMIT);
        assert_eq!(state.order.len(), RESIDENT_LIMIT);
    }

    #[test]
    fn propagated_preview_refreshes_recent_and_pins_only_replies() {
        for pins_only in [false, true] {
            let mut state = RichChat::default();
            let target = message(1, 2);
            let mut reply = pinned(2, 3);
            reply.reply = Some(ReplyPreview {
                target_revision: Some(1),
                id: target.id,
                preview: Some("old preview".into()),
            });
            reaction(&mut reply, true);
            let pins = state.begin_pins();
            state.finish_pins(pins, vec![reply.clone()], |_| None);
            for id in 3..=if pins_only { 502 } else { 500 } {
                state.reconcile(vec![message(id, 1)], |_| None);
            }
            assert_eq!(state.recent.contains_key(&reply.id), !pins_only);
            let history = state.begin_response();
            state.reconcile(vec![target.clone()], |_| None);
            state.close_pins();
            assert_eq!(state.response_status(history), SnapshotStatus::Ready);
            let (clock, retained) = &state.recent[&reply.id];
            assert!(*clock > history.since);
            assert_eq!(retained.reply.as_ref().unwrap().preview, preview(&target));
            assert_eq!(retained.revision, reply.revision);
            assert!(retained.reactions[0].mine);
            // Identical payloads must not extend the clock/retention lifetime.
            let clock = state.clock;
            state.reconcile(vec![target], |_| None);
            assert_eq!(state.clock, clock);
        }
    }

    #[test]
    fn fresh_pins_preview_wins_over_intervening_equal_revision_preview_update() {
        let mut state = RichChat::default();
        let mut reply = pinned(2, 3);
        reply.reply = Some(ReplyPreview {
            target_revision: Some(1),
            id: message(1, 1).id,
            preview: Some("old preview".into()),
        });
        state.reconcile(vec![reply.clone()], |_| None);
        let pins = state.begin_pins();
        reply.reply.as_mut().unwrap().preview = Some("intervening preview".into());
        reply.reply.as_mut().unwrap().target_revision = Some(2);
        state.reconcile(vec![reply.clone()], |_| None);
        reply.reply.as_mut().unwrap().preview = Some("fresh server preview".into());
        reply.reply.as_mut().unwrap().target_revision = Some(3);
        assert_eq!(
            state.finish_pins(pins, vec![reply], |_| None),
            SnapshotStatus::Ready
        );
        assert_eq!(
            state.pins()[0].reply.as_ref().unwrap().preview.as_deref(),
            Some("fresh server preview")
        );
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
        let edit = state.begin_response();
        let reaction_request = state.begin_response();
        let history = state.begin_response();
        let tombstone = deleted(1, 4);
        let updates = state.reconcile(vec![tombstone], |_| None);
        cache_merge(&mut cache, updates, Merge::Older);
        // Exercise live reconciliation up to the eviction boundary first.
        for id in 2..=500 {
            let updates = state.reconcile(vec![message(id, 1)], |id| cache.get(id).cloned());
            cache_merge(&mut cache, updates, Merge::Live { follow: true });
        }
        let mut delayed_reaction = old.clone();
        delayed_reaction.revision = 3;
        reaction(&mut delayed_reaction, true);
        for delayed in [old.clone(), delayed_reaction] {
            let delivered = state.reconcile(vec![delayed], |id| cache.get(id).cloned());
            let accepted = &delivered[0];
            assert!(accepted.deleted);
            assert_eq!(accepted.revision, 4);
            assert_eq!(preview(accepted), None);
            assert!(accepted.content.is_empty() && accepted.attachments.is_empty());
            assert!(!accepted.pinned);
            assert!(state.pins().is_empty());
        }
        // The next real event evicts the tombstone from BOTH bounded stores.
        // Older-history and action responses must now be rejected before merge.
        let updates = state.reconcile(vec![message(501, 1)], |id| cache.get(id).cloned());
        cache_merge(&mut cache, updates, Merge::Live { follow: true });
        assert!(cache.get(old.id).is_none());
        assert!(!state.recent.contains_key(&old.id));
        for ticket in [edit, reaction_request, history] {
            assert_eq!(state.response_status(ticket), SnapshotStatus::Overflow);
        }
        assert!(state.pins().is_empty());
        assert_eq!(state.recent.len(), RESIDENT_LIMIT);
        assert_eq!(state.order.len(), RESIDENT_LIMIT);
        // A retried read starts a new watermark and can use the current snapshot.
        let retry = state.begin_response();
        assert_eq!(state.response_status(retry), SnapshotStatus::Ready);
        let updates = state.reconcile(vec![deleted(1, 4)], |id| cache.get(id).cloned());
        cache_merge(&mut cache, updates, Merge::Older);
        assert!(cache.get(old.id).unwrap().deleted);
    }

    #[test]
    fn older_history_outside_resident_window_overflows_after_real_live_events() {
        let mut state = RichChat::default();
        let mut cache = MessageCache::default();
        let initial = state.reconcile(vec![message(1000, 1)], |_| None);
        cache_merge(&mut cache, initial, Merge::Older);
        let history = state.begin_response();
        let updates = state.reconcile(vec![deleted(1, 2)], |id| cache.get(id).cloned());
        cache_merge(&mut cache, updates, Merge::Live { follow: false });
        assert!(cache.get(message(1, 1).id).is_none());
        for id in 1001..=1501 {
            let updates = state.reconcile(vec![message(id, 1)], |id| cache.get(id).cloned());
            cache_merge(&mut cache, updates, Merge::Live { follow: false });
        }
        assert!(!state.recent.contains_key(&message(1, 1).id));
        assert_eq!(state.response_status(history), SnapshotStatus::Overflow);
        assert!(cache.newer);
        assert_eq!(cache.values().count(), RESIDENT_LIMIT);
        assert_eq!(state.recent.len(), RESIDENT_LIMIT);
    }

    #[test]
    fn pinned_reply_keeps_target_updates_after_tombstone_eviction_and_context_jump() {
        for target in [message(1, 2), deleted(1, 2)] {
            let mut state = RichChat::default();
            let mut reply = pinned(2, 3);
            reply.reply = Some(ReplyPreview {
                target_revision: Some(1),
                id: target.id,
                preview: Some("old private text".into()),
            });
            let pins = state.begin_pins();
            state.finish_pins(pins, vec![reply.clone()], |_| None);
            // Both the recent and pinned copies must be normalized immediately,
            // even though neither the reply nor its target is a resident row.
            state.reconcile(vec![target.clone()], |_| None);
            let expected = preview(&target);
            assert_eq!(state.pins()[0].reply.as_ref().unwrap().preview, expected);
            assert_eq!(
                state.recent[&reply.id].1.reply.as_ref().unwrap().preview,
                expected
            );
            for id in 3..=503 {
                state.reconcile(vec![message(id, 1)], |_| None);
            }
            assert!(!state.recent.contains_key(&target.id));
            assert!(!state.recent.contains_key(&reply.id));
            assert_eq!(state.pins()[0].reply.as_ref().unwrap().preview, expected);
            let context = state.begin_context();
            reply.reply.as_mut().unwrap().preview = expected.clone();
            assert_eq!(state.finish_context(context), SnapshotStatus::Ready);
            let snapshot = state.reconcile(vec![reply], |_| None);
            let mut cache = MessageCache::default();
            cache_merge(&mut cache, snapshot, Merge::Older);
            assert_eq!(
                cache
                    .get(message(2, 1).id)
                    .unwrap()
                    .reply
                    .as_ref()
                    .unwrap()
                    .preview,
                expected
            );
        }
    }

    #[test]
    fn equal_revision_pin_cannot_replace_fresher_snapshot_preview() {
        let mut state = RichChat::default();
        let mut reply = pinned(2, 3);
        reply.reply = Some(ReplyPreview {
            target_revision: Some(1),
            id: message(1, 1).id,
            preview: Some("old preview".into()),
        });
        let pins = state.begin_pins();
        state.finish_pins(pins, vec![reply.clone()], |_| None);
        for id in 3..=503 {
            state.reconcile(vec![message(id, 1)], |_| None);
        }
        // HTTP can observe a target change before its socket event arrives.
        // The reply's own revision stays equal to the retained pin's revision.
        for preview in [Some("edited preview".into()), None] {
            let context = state.begin_context();
            reply.reply.as_mut().unwrap().preview = preview.clone();
            reply.reply.as_mut().unwrap().target_revision = Some(2);
            assert_eq!(state.finish_context(context), SnapshotStatus::Ready);
            let snapshot = state.reconcile(vec![reply.clone()], |_| None);
            assert_eq!(snapshot[0].reply.as_ref().unwrap().preview, preview);
            assert_eq!(state.pins()[0].reply.as_ref().unwrap().preview, preview);
            assert_eq!(
                state.recent[&reply.id].1.reply.as_ref().unwrap().preview,
                preview
            );
        }
        // A delayed action can advance the reply's own revision without knowing
        // that its target was deleted. No target tombstone is needed while any
        // retained reply already knows the target is unavailable.
        reply.revision += 1;
        reply.reply.as_mut().unwrap().preview = Some("stale action preview".into());
        let snapshot = state.reconcile(vec![reply], |_| None);
        assert_eq!(snapshot[0].reply.as_ref().unwrap().preview, None);
        assert_eq!(state.pins()[0].reply.as_ref().unwrap().preview, None);
    }

    #[test]
    fn reaction_response_for_reply_cannot_restore_its_independently_deleted_target() {
        let mut state = RichChat::default();
        state.reconcile(vec![deleted(1, 3)], |_| None);
        let mut reply = message(2, 8);
        reply.reply = Some(ReplyPreview {
            target_revision: Some(1),
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
        let old_response = state.begin_response();
        state.reset();
        assert!(state.recent.is_empty() && state.pins().is_empty());
        let new_context = state.begin_context();
        let new_pins = state.begin_pins();
        let new_response = state.begin_response();
        assert_eq!(
            state.response_status(old_response),
            SnapshotStatus::Cancelled
        );
        assert_eq!(state.response_status(new_response), SnapshotStatus::Ready);
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
