//! HTTP pages and live snapshots may complete in either order.
use thiscord_shared::friends::DirectMessage;

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

#[cfg(test)]
mod tests {
    use super::*;

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
