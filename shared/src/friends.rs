//! Private social contracts. Deliberately no Debug on messages or token frames.
use crate::ids::{CallId, ConversationId};
use crate::pagination::{PageCursor, PageSize};
use crate::{AccountId, ApiError, AvatarId, ClientMessageId, MessageId, Timestamp};
use serde::{Deserialize, Serialize};

pub const FRIENDS_PATH: &str = "/api/v1/friends";
pub const FRIENDS_SOCKET_PATH: &str = "/api/v1/friends/socket";
pub const VERSION: u8 = 1;
pub const MAX_RELATIONSHIPS: usize = 200;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: AccountId,
    pub username: String,
    pub display_name: String,
    pub avatar_id: Option<AvatarId>,
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
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectMessage {
    pub id: MessageId,
    pub conversation_id: ConversationId,
    pub client_id: ClientMessageId,
    pub author_id: AccountId,
    pub content: String,
    pub created_at: Timestamp,
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
    pub history: Option<History>,
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
    },
    Ping {},
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
