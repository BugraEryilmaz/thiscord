use crate::{
    AccountId, ApiError, ChannelId, ClientMessageId, GuildId, MessageId, Timestamp,
    pagination::{PageCursor, PageSize},
};
use serde::{Deserialize, Serialize};
pub const CHAT_PATH: &str = "/api/v1/chat";
pub const SOCKET_PATH: &str = "/api/v1/socket";
pub const SOCKET_VERSION: u8 = 1;
pub const MAX_MESSAGE_CHARS: usize = 4000;
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub client_id: ClientMessageId,
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub author_id: Option<AccountId>,
    pub username: String,
    pub content: String,
    pub mentions: Vec<AccountId>,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub deleted: bool,
    pub revision: i32,
    pub sequence: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct History {
    pub messages: Vec<ChatMessage>,
    pub older: Option<PageCursor>,
    pub event_cursor: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Unread {
    pub channel_id: ChannelId,
    pub count: i64,
    pub mentions: i64,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnlineMember {
    pub account_id: AccountId,
    pub username: String,
    pub typing: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatRequest {
    History {
        guild_id: GuildId,
        channel_id: ChannelId,
        #[serde(default)]
        before: Option<PageCursor>,
        #[serde(default)]
        limit: PageSize,
    },
    Send {
        guild_id: GuildId,
        channel_id: ChannelId,
        client_id: ClientMessageId,
        content: String,
    },
    Edit {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
        revision: i32,
        content: String,
    },
    Delete {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
        revision: i32,
    },
    Read {
        guild_id: GuildId,
        channel_id: ChannelId,
        through: i64,
    },
    Unread {
        guild_id: GuildId,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ChatResponse {
    History { history: History },
    Message { message: ChatMessage },
    Unread { channels: Vec<Unread> },
    Done,
}
// Never Debug: the first frame carries a bearer token.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Authenticate {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Ping {},
    Typing {
        active: bool,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Ready { history: History },
    Message { message: ChatMessage, cursor: i64 },
    Presence { members: Vec<OnlineMember> },
    Pong {},
    Revoked {},
    Error { error: ApiError },
}
