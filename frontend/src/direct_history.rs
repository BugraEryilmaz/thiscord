//! HTTP pages and live snapshots may complete in either order.
use thiscord_shared::friends::{DirectMessage, History, Snapshot};
use thiscord_shared::{MessageId, Timestamp, ids::ConversationId, pagination::PageCursor};

/// History's upper bound is exclusive. Its immediate successor includes exactly
/// this message, even when many messages share a PostgreSQL timestamp.
pub fn before_including(at: Timestamp, id: MessageId) -> Option<PageCursor> {
    let (at, next) = match id.as_uuid().as_u128().checked_add(1) {
        Some(next) => (at, next),
        None => (
            Timestamp::from_timestamp_micros(at.timestamp_micros().checked_add(1)?)?,
            0,
        ),
    };
    Some(PageCursor::new(at, format!("{next:032x}").parse().ok()?))
}

pub fn preserve_revisions(incoming: &mut [DirectMessage], current: &[DirectMessage]) {
    for message in incoming {
        if let Some(newer) = current.iter().find(|old| {
            old.id == message.id
                && old.conversation_id == message.conversation_id
                && old.revision > message.revision
        }) {
            *message = newer.clone();
        }
    }
}

/// The last history received on one friends socket, keyed by selection and page.
pub type Received = Option<(ConversationId, Option<PageCursor>, History)>;

/// A server that honours `RETAINED_HISTORY_QUERY` omits an unchanged history after
/// delivering it on the same socket. Restore it so a snapshot never loses history;
/// a snapshot for another selection or page never receives this one.
pub fn retain(snapshot: &mut Snapshot, received: &mut Received) {
    let Some(id) = snapshot.conversation_id else {
        *received = None;
        return;
    };
    match &snapshot.history {
        Some(history) => *received = Some((id, snapshot.before.clone(), history.clone())),
        None => {
            if let Some((at, before, history)) = received
                && *at == id
                && *before == snapshot.before
            {
                snapshot.history = Some(history.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_history_is_restored_only_for_the_same_selection_and_page() {
        let id: ConversationId = "00000000-0000-4000-8000-000000000002".parse().unwrap();
        let other: ConversationId = "00000000-0000-4000-8000-000000000005".parse().unwrap();
        let message: DirectMessage = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-4000-8000-000000000001",
            "conversation_id":id,
            "client_id":"00000000-0000-4000-8000-000000000003",
            "author_id":"00000000-0000-4000-8000-000000000004",
            "content":"kept", "created_at":"2026-10-23T00:00:00Z"
        }))
        .unwrap();
        let history = History {
            messages: vec![message],
            older: None,
        };
        let mut received = None;
        let mut full = Snapshot {
            conversation_id: Some(id),
            history: Some(history.clone()),
            ..Default::default()
        };
        retain(&mut full, &mut received);
        assert!(full.history == Some(history.clone()));
        let mut omitted = Snapshot {
            conversation_id: Some(id),
            ..Default::default()
        };
        retain(&mut omitted, &mut received);
        assert!(omitted.history == Some(history.clone()));
        // Another page or conversation never borrows this history.
        let mut paged = omitted.clone();
        paged.history = None;
        paged.before = Some(PageCursor::new(
            Timestamp::from_timestamp_micros(1).unwrap(),
            id.as_uuid(),
        ));
        retain(&mut paged, &mut received);
        assert!(paged.history.is_none());
        let mut elsewhere = Snapshot {
            conversation_id: Some(other),
            ..Default::default()
        };
        retain(&mut elsewhere, &mut received);
        assert!(elsewhere.history.is_none());
        // Losing access (no selection) forgets the retained history.
        retain(&mut Snapshot::default(), &mut received);
        assert!(received.is_none());
        let mut later = Snapshot {
            conversation_id: Some(id),
            ..Default::default()
        };
        retain(&mut later, &mut received);
        assert!(later.history.is_none());
    }

    #[test]
    fn jump_cursor_includes_target_without_later_timestamp_ties() {
        let at = Timestamp::from_timestamp_micros(123456).unwrap();
        for value in [
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-0000000000ff",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            let id: MessageId = value.parse().unwrap();
            let (next_at, next_id) = before_including(at, id).unwrap().position();
            assert!((at, id.as_uuid()) < (next_at, next_id));
            if let Some(next) = id.as_uuid().as_u128().checked_add(1) {
                assert_eq!(next_at, at);
                assert_eq!(next_id.as_u128(), next);
            } else {
                assert_eq!(next_at.timestamp_micros(), at.timestamp_micros() + 1);
                assert_eq!(next_id.as_u128(), 0);
            }
        }
    }

    #[test]
    fn late_history_cannot_resurrect_live_tombstone_or_undo_edit() {
        let original: DirectMessage = serde_json::from_value(serde_json::json!({
            "id":"00000000-0000-4000-8000-000000000001",
            "conversation_id":"00000000-0000-4000-8000-000000000002",
            "client_id":"00000000-0000-4000-8000-000000000003",
            "author_id":"00000000-0000-4000-8000-000000000004",
            "content":"original", "created_at":"2026-10-23T00:00:00Z"
        }))
        .unwrap();
        let mut edited = original.clone();
        edited.revision = 2;
        edited.content = "edited".into();
        let mut late = vec![original.clone()];
        preserve_revisions(&mut late, &[edited.clone()]);
        assert_eq!(late[0].content, "edited");
        let mut deleted = edited.clone();
        deleted.revision = 3;
        deleted.deleted = true;
        deleted.content.clear();
        preserve_revisions(&mut late, &[deleted.clone()]);
        assert!(late[0].deleted);
        assert!(late[0].content.is_empty());
        // A newer authoritative page still advances the row.
        let mut fresh = vec![deleted];
        preserve_revisions(&mut fresh, &[edited]);
        assert_eq!(fresh[0].revision, 3);
        // Page composition is unchanged; only overlapping revisions are merged.
        let mut another = original.clone();
        another.id = thiscord_shared::MessageId::from_uuid(another.client_id.as_uuid());
        let mut page = vec![another];
        preserve_revisions(&mut page, &[original]);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].revision, 1);
    }
}
