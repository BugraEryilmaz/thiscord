//! Private social contracts. Deliberately no Debug on messages or token frames.
use crate::ids::{CallId, ConversationId};
use crate::pagination::{PageCursor, PageSize};
use crate::{AccountId, ApiError, AttachmentId, AvatarId, ClientMessageId, MessageId, Timestamp};
use serde::{Deserialize, Serialize};

pub const FRIENDS_PATH: &str = "/api/v1/friends";
pub const FRIENDS_SOCKET_PATH: &str = "/api/v1/friends/socket";
/// Socket query opt-in: the client keeps the history it last received on this
/// socket, so the server may omit an unchanged history (see `Snapshot::history`).
/// Older servers ignore the query; older clients never send it.
pub const RETAINED_HISTORY_QUERY: &str = "history=retained";
pub const VERSION: u8 = 1;
pub const MAX_RELATIONSHIPS: usize = 200;
pub const DIRECT_ATTACHMENTS_PATH: &str = "/api/v1/direct-attachments";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectAttachmentUpload {
    pub conversation_id: ConversationId,
    pub filename: String,
    pub content_type: String,
}

fn initial_revision() -> i32 {
    1
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: AccountId,
    pub username: String,
    pub display_name: String,
    pub avatar_id: Option<AvatarId>,
    /// Accepted friends only; never in search results, requests or across a block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<crate::account::CustomStatus>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Incoming,
    Outgoing,
    Friend,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friend {
    pub conversation_id: ConversationId,
    pub person: Person,
    pub relationship: Relationship,
    pub online: bool,
    /// What an online accepted friend is doing, reported live by one of their
    /// desktop clients. Absent while offline, idle or from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::account::Activity>,
    /// Incoming unread messages, capped at 100 (display as 99+).
    /// Defaults to zero when connected to an older server.
    #[serde(default)]
    pub unread_count: u32,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectMessage {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub client_id: ClientMessageId,
    pub author_id: AccountId,
    pub content: String,
    pub created_at: Timestamp,
    #[serde(default = "initial_revision")]
    pub revision: i32,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub edited_at: Option<Timestamp>,
    #[serde(default)]
    pub attachments: Vec<crate::chat::ChatAttachment>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub messages: Vec<DirectMessage>,
    pub older: Option<PageCursor>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Ringing,
    Active,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: CallId,
    pub conversation_id: ConversationId,
    pub caller: AccountId,
    pub callee: AccountId,
    pub state: CallState,
}
impl Call {
    /// Dedicated SFU namespace: equal scope/room UUIDs identify direct calls.
    /// These are transport keys, never guilds or guild membership grants.
    pub fn voice_scope(&self) -> crate::GuildId {
        crate::GuildId::from_uuid(self.id.as_uuid())
    }
    pub fn voice_channel(&self) -> crate::ChannelId {
        crate::ChannelId::from_uuid(self.id.as_uuid())
    }
}
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub friends: Vec<Friend>,
    pub calls: Vec<Call>,
    pub conversation_id: Option<ConversationId>,
    /// Always present with a selected conversation, except on sockets opened with
    /// `RETAINED_HISTORY_QUERY`: there `None` alongside `Some(conversation_id)`
    /// means unchanged since the history this socket last delivered for the same
    /// conversation and `before` page. Every new socket or `Select` starts with a
    /// full history.
    pub history: Option<History>,
    #[serde(default)]
    pub before: Option<PageCursor>,
    #[serde(default)]
    pub typing: Vec<AccountId>,
    /// The connected account's own unexpired custom status, synchronizing it
    /// live across that account's devices. Absent when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_status: Option<crate::account::CustomStatus>,
    /// The server accepts [`ClientEvent::Activity`] on this socket. Older servers
    /// omit it and close sockets that send unknown events, so clients wait for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_activity: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Case-insensitive literal substring of a display name or username.
    /// Trimmed input must contain 2–64 Unicode characters and no controls.
    Search {
        query: String,
    },
    Add {
        account_id: AccountId,
    },
    Accept {
        account_id: AccountId,
    },
    Remove {
        account_id: AccountId,
    },
    History {
        conversation_id: ConversationId,
        #[serde(default)]
        before: Option<PageCursor>,
        #[serde(default)]
        limit: PageSize,
    },
    Send {
        conversation_id: ConversationId,
        client_id: ClientMessageId,
        content: String,
        #[serde(default)]
        attachments: Vec<AttachmentId>,
    },
    Edit {
        conversation_id: ConversationId,
        message_id: MessageId,
        revision: i32,
        content: String,
    },
    Delete {
        conversation_id: ConversationId,
        message_id: MessageId,
        revision: i32,
    },
    /// Acknowledge only messages through an actually displayed message.
    Read {
        conversation_id: ConversationId,
        through: MessageId,
    },
    StartCall {
        conversation_id: ConversationId,
    },
    AcceptCall {
        call_id: CallId,
    },
    EndCall {
        call_id: CallId,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    People { people: Vec<Person> },
    History { history: History },
    Message { message: DirectMessage },
    Call { call: Call },
    Done,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Connect {
        token: String,
    },
    Select {
        conversation_id: Option<ConversationId>,
        #[serde(default)]
        before: Option<PageCursor>,
    },
    Ping {},
    Typing {
        active: bool,
    },
    /// Replaces this socket's live activity; null clears it. It ends with the
    /// socket. Send only after a snapshot with `accepts_activity`.
    Activity {
        activity: Option<crate::account::Activity>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Snapshot { snapshot: Snapshot },
    Pong {},
    Revoked {},
    Error { error: ApiError },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_private_messages_and_sends_default_additive_fields() {
        let id = "00000000-0000-0000-0000-000000000001";
        let old = serde_json::json!({"id":id,"conversation_id":id,"client_id":id,"author_id":id,"content":"hello","created_at":"2026-10-23T14:00:00Z"});
        let message: DirectMessage = serde_json::from_value(old).unwrap();
        assert_eq!(message.revision, 1);
        assert!(!message.deleted);
        assert!(message.edited_at.is_none());
        assert!(message.attachments.is_empty());
        let send:Request=serde_json::from_value(serde_json::json!({"action":"send","conversation_id":id,"client_id":id,"content":"hello"})).unwrap();
        assert!(matches!(send,Request::Send {attachments,..} if attachments.is_empty()));
        let select: ClientEvent =
            serde_json::from_value(serde_json::json!({"type":"select","conversation_id":id}))
                .unwrap();
        assert!(matches!(select, ClientEvent::Select { before: None, .. }));
        let snapshot: Snapshot = serde_json::from_value(
            serde_json::json!({"friends":[],"calls":[],"conversation_id":null,"history":null}),
        )
        .unwrap();
        assert!(snapshot.typing.is_empty());
        assert!(snapshot.before.is_none());
        assert!(snapshot.own_status.is_none());
        assert!(!snapshot.accepts_activity);
        assert!(
            serde_json::to_value(&snapshot)
                .unwrap()
                .get("accepts_activity")
                .is_none()
        );
        let person: Person = serde_json::from_value(
            serde_json::json!({"id":id,"username":"u","display_name":"U","avatar_id":null}),
        )
        .unwrap();
        assert!(person.custom_status.is_none());
        assert!(
            serde_json::to_value(&person)
                .unwrap()
                .get("custom_status")
                .is_none()
        );
    }
    #[test]
    fn unread_counts_are_additive_and_read_commands_are_typed() {
        let id = "00000000-0000-0000-0000-000000000001";
        let mut legacy = serde_json::json!({
            "conversation_id": id,
            "person": {"id": id, "username": "friend", "display_name": "Friend", "avatar_id": null},
            "relationship": "friend", "online": false
        });
        let friend: Friend = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(friend.unread_count, 0);
        assert!(friend.activity.is_none());
        assert!(
            serde_json::to_value(&friend)
                .unwrap()
                .get("activity")
                .is_none()
        );
        legacy["unread_count"] = serde_json::json!(100);
        let friend: Friend = serde_json::from_value(legacy).unwrap();
        assert_eq!(serde_json::to_value(friend).unwrap()["unread_count"], 100);
        let wire = serde_json::json!({"action":"read", "conversation_id":id, "through":id});
        let command: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), wire);
        assert!(
            serde_json::from_value::<Request>(serde_json::json!({
                "action":"read", "conversation_id":id, "through":"tomorrow"
            }))
            .is_err()
        );
    }

    #[test]
    fn activity_events_are_typed() {
        let wire =
            serde_json::json!({"type":"activity","activity":{"kind":"playing","name":"Factorio"}});
        let event: ClientEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(
            matches!(&event, ClientEvent::Activity { activity: Some(a) } if a.name == "Factorio")
        );
        assert_eq!(serde_json::to_value(event).unwrap(), wire);
        let clear: ClientEvent =
            serde_json::from_value(serde_json::json!({"type":"activity","activity":null})).unwrap();
        assert!(matches!(clear, ClientEvent::Activity { activity: None }));
        assert!(
            serde_json::from_value::<ClientEvent>(
                serde_json::json!({"type":"activity","activity":null,"account_id":"x"})
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unbounded_history_and_unknown_fields() {
        let id = "00000000-0000-0000-0000-000000000001";
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"action":"history","conversation_id":id,"limit":101})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"action":"add","account_id":id,"admin":true})
            )
            .is_err()
        );
        let request: Request =
            serde_json::from_value(serde_json::json!({"action":"history","conversation_id":id}))
                .unwrap();
        assert_eq!(serde_json::to_value(request).unwrap()["limit"], 50);
    }
}
