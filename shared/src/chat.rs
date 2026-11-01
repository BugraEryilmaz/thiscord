use crate::{
    AccountId, ApiError, AttachmentId, AvatarId, ChannelId, ClientMessageId, GuildId, MessageId,
    Timestamp,
    pagination::{PageCursor, PageSize},
};
use serde::{Deserialize, Serialize};
pub const CHAT_PATH: &str = "/api/v1/chat";
pub const SOCKET_PATH: &str = "/api/v1/socket";
pub const SOCKET_VERSION: u8 = 1;
pub const MAX_MESSAGE_CHARS: usize = 4000;
pub const MAX_MENTION_QUERY_CHARS: usize = 64;
pub const MENTION_SUGGESTION_LIMIT: usize = 8;
/// Quick reactions shown first. Every entry is also part of [`EMOJI_GROUPS`].
pub const REACTION_EMOJI: &[&str] = &["👍", "👎", "❤️", "😂", "🎉", "😮", "😢", "🔥", "✅", "👀"];
/// A bounded, interoperable Unicode emoji palette for the picker and reactions.
/// No custom image or shortcode URLs. Labels are English UI catalog keys.
pub const EMOJI_GROUPS: &[(&str, &[&str])] = &[
    (
        "Smileys",
        &[
            "😀", "😃", "😄", "😁", "😆", "😅", "🤣", "😂", "🙂", "😉", "😊", "😇", "🥰", "😍",
            "🤩", "😘", "😋", "😛", "😜", "🤪", "🤔", "🤨", "😐", "😑", "😶", "🙄", "😏", "😬",
            "😌", "😔", "😴", "😷", "🤒", "🤯", "🥳", "😎", "🤓", "😮", "😲", "😳", "🥺", "😢",
            "😭", "😱", "😤", "😡", "🤬", "💀", "🤡", "👻",
        ],
    ),
    (
        "People",
        &[
            "👍", "👎", "👌", "✌️", "🤞", "🤟", "🤘", "👋", "👏", "🙌", "🙏", "💪", "👀", "🤝",
            "✋", "👉", "👈", "👆", "👇", "🤷", "🤦",
        ],
    ),
    (
        "Nature",
        &[
            "🐶", "🐱", "🐭", "🦊", "🐻", "🐼", "🐸", "🐵", "🐔", "🐧", "🦄", "🐝", "🐢", "🐍",
            "🌸", "🌻", "🌲", "🌈", "☀️", "🌙", "⚡", "❄️", "🌊",
        ],
    ),
    (
        "Food",
        &[
            "🍕", "🍔", "🍟", "🌭", "🍿", "🥐", "🍩", "🍪", "🎂", "🍰", "🍫", "🍎", "🍉", "🍓",
            "🍌", "☕", "🍵", "🍺", "🍻", "🥂", "🍷",
        ],
    ),
    (
        "Activities",
        &[
            "🎉", "🎊", "🎁", "🎈", "🏆", "🥇", "⚽", "🏀", "🎮", "🕹️", "🎲", "🎧", "🎤", "🎵",
            "🎸", "🎬",
        ],
    ),
    (
        "Objects",
        &[
            "📷", "💻", "🖥️", "⌨️", "📱", "💡", "📌", "📎", "🔒", "🔑", "🔔", "🔕", "⏰", "💰",
            "🚀", "✈️", "🚗", "🏠",
        ],
    ),
    (
        "Symbols",
        &[
            "❤️", "🧡", "💛", "💚", "💙", "💜", "🖤", "🤍", "💔", "💯", "✅", "❌", "❓", "❗",
            "⭐", "✨", "💥", "💤", "💬", "🔥", "🇹🇷",
        ],
    ),
];
/// Distinct reaction emoji per message; repeats of existing reactions remain allowed.
pub const MAX_REACTION_KINDS: usize = 20;
pub fn is_reaction_emoji(emoji: &str) -> bool {
    EMOJI_GROUPS.iter().any(|(_, group)| group.contains(&emoji))
}
pub const MAX_CHANNEL_PINS: usize = 50;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub count: i64,
    pub mine: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyPreview {
    pub id: MessageId,
    /// None means deleted/unavailable; no author or content is retained.
    pub preview: Option<String>,
    /// Revision of the target, independent of the containing reply's revision.
    /// Missing on legacy servers or when the recipient cannot read the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<i32>,
}
pub const ATTACHMENTS_PATH: &str = "/api/v1/attachments";
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_MESSAGE_ATTACHMENTS: usize = 4;
pub const MAX_ATTACHMENT_FILENAME_BYTES: usize = 255;
pub const MAX_STAGED_ATTACHMENT_BYTES: usize = 100 * 1024 * 1024;
pub const MAX_STAGED_ATTACHMENTS: usize = 16;
pub const ATTACHMENT_STAGE_TTL_SECONDS: u64 = 3600;

/// Native WebView download completion, delivered only to its owning window.
pub const CHAT_DOWNLOAD_EVENT: &str = "thiscord-download-finished";
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatDownload {
    pub url: String,
    pub success: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatAttachment {
    pub id: AttachmentId,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: u64,
}

/// Raw binary POST query. The server classifies content independently of this hint.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentUpload {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub filename: String,
    pub content_type: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub client_id: ClientMessageId,
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub author_id: Option<AccountId>,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    /// Current profile picture of the author. Missing on older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_avatar_id: Option<AvatarId>,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    pub mentions: Vec<AccountId>,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub deleted: bool,
    pub revision: i32,
    pub sequence: i64,
    #[serde(default)]
    pub reply: Option<ReplyPreview>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub pinned: bool,
}
impl ChatMessage {
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct History {
    pub messages: Vec<ChatMessage>,
    pub older: Option<PageCursor>,
    pub event_cursor: i64,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unread {
    pub channel_id: ChannelId,
    pub count: i64,
    pub mentions: i64,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnlineMember {
    pub account_id: AccountId,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    pub typing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<AvatarId>,
}
impl OnlineMember {
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChatRequest {
    MentionSuggestions {
        guild_id: GuildId,
        channel_id: ChannelId,
        query: String,
    },
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachment_ids: Vec<AttachmentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<MessageId>,
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
    React {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
        emoji: String,
        present: bool,
    },
    Pin {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
        revision: i32,
        pinned: bool,
    },
    Pins {
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Context {
        guild_id: GuildId,
        channel_id: ChannelId,
        message_id: MessageId,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ChatResponse {
    MentionSuggestions { members: Vec<MentionMember> },
    History { history: History },
    Message { message: Box<ChatMessage> },
    Unread { channels: Vec<Unread> },
    Pins { messages: Vec<ChatMessage> },
    Done,
}

/// Public identity only; candidates include offline members of the current guild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionMember {
    pub account_id: AccountId,
    pub username: String,
    pub display_name: String,
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
    Connect {
        token: String,
    },
    Subscribe {
        subscription: u64,
        guild_id: Option<GuildId>,
        channel_id: Option<ChannelId>,
        /// Opt in so older clients never receive an unknown event variant.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        voice_roster: bool,
    },
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
    Authenticated {},
    Subscribed {
        subscription: u64,
        history: Option<History>,
        permissions: crate::permissions::Permissions,
    },
    Update {
        subscription: u64,
        event: Box<ServerEvent>,
    },
    Unread {
        channels: Vec<Unread>,
    },
    VoiceRoster {
        channels: Vec<crate::voice::ChannelRoster>,
    },
    Ready {
        history: History,
    },
    Message {
        message: Box<ChatMessage>,
        cursor: i64,
    },
    Presence {
        members: Vec<OnlineMember>,
    },
    Pong {},
    Revoked {},
    Error {
        error: ApiError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mention_suggestions_use_public_identity_and_stable_tags() {
        let id = "00000000-0000-0000-0000-000000000001";
        let wire = json!({"action":"mention_suggestions", "guild_id":id, "channel_id":id, "query":"Ada Lov"});
        let request: ChatRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), wire);
        let response = json!({"result":"mention_suggestions", "members":[{
            "account_id":id, "username":"ada_2", "display_name":"Ada Lovelace"
        }]});
        let parsed: ChatResponse = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), response);
    }

    #[test]
    fn reply_target_revision_is_additive_and_independent() {
        let legacy = json!({"id":"00000000-0000-0000-0000-000000000001","preview":"old"});
        let mut preview: ReplyPreview = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(preview.target_revision, None);
        assert_eq!(serde_json::to_value(&preview).unwrap(), legacy);
        for revision in [0, 42] {
            preview.target_revision = Some(revision);
            let wire = serde_json::to_value(&preview).unwrap();
            assert_eq!(wire["target_revision"], revision);
            let decoded: ReplyPreview = serde_json::from_value(wire).unwrap();
            assert!(decoded == preview);
        }
    }

    #[test]
    fn legacy_send_omits_reply_and_old_messages_default_rich_fields() {
        let id = "00000000-0000-0000-0000-000000000001";
        let request: ChatRequest = serde_json::from_value(
            json!({"action":"send","guild_id":id,"channel_id":id,"client_id":id,"content":"hello"}),
        )
        .unwrap();
        assert!(matches!(request, ChatRequest::Send { reply_to: None, .. }));
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .get("reply_to")
                .is_none()
        );
        let message:ChatMessage=serde_json::from_value(json!({"id":id,"client_id":id,"guild_id":id,"channel_id":id,"author_id":null,"username":"old","content":"hello","mentions":[],"created_at":"2026-01-01T00:00:00Z","edited_at":null,"deleted":false,"revision":0,"sequence":1})).unwrap();
        assert!(message.reply.is_none() && message.reactions.is_empty() && !message.pinned);
        assert!(message.author_avatar_id.is_none());
        assert!(
            serde_json::to_value(&message)
                .unwrap()
                .get("author_avatar_id")
                .is_none()
        );
    }

    #[test]
    fn author_and_presence_avatars_are_additive() {
        let id = "00000000-0000-0000-0000-000000000001";
        let message: ChatMessage = serde_json::from_value(json!({"id":id,"client_id":id,"guild_id":id,"channel_id":id,"author_id":id,"username":"u","author_avatar_id":id,"content":"hi","mentions":[],"created_at":"2026-01-01T00:00:00Z","edited_at":null,"deleted":false,"revision":0,"sequence":1})).unwrap();
        assert_eq!(message.author_avatar_id.unwrap().to_string(), id);
        let member: OnlineMember = serde_json::from_value(
            json!({"account_id":id,"username":"u","typing":false,"avatar_id":id}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&member).unwrap()["avatar_id"], id);
    }

    #[test]
    fn emoji_palette_is_bounded_unique_and_contains_quick_reactions() {
        let mut seen = std::collections::HashSet::new();
        for (label, group) in EMOJI_GROUPS {
            assert!(!label.is_empty() && !group.is_empty());
            for emoji in *group {
                // Mirrors the message_reactions length constraint.
                assert!((1..=16).contains(&emoji.chars().count()), "{emoji}");
                assert!(seen.insert(*emoji), "duplicate {emoji}");
            }
        }
        for emoji in REACTION_EMOJI {
            assert!(is_reaction_emoji(emoji), "{emoji}");
        }
        assert!(!is_reaction_emoji("x") && !is_reaction_emoji("") && !is_reaction_emoji("👍👍"));
    }

    #[test]
    fn profile_names_preserve_handles_and_accept_legacy_presence() {
        let legacy = json!({
            "account_id": "00000000-0000-0000-0000-000000000001",
            "username": "user_123",
            "typing": false
        });
        let mut member: OnlineMember = serde_json::from_value(legacy).unwrap();
        assert_eq!(member.display_name(), "user_123");
        member.display_name = "Büğra 🎮".into();
        let wire = serde_json::to_value(&member).unwrap();
        assert_eq!(wire["username"], "user_123");
        assert_eq!(wire["display_name"], "Büğra 🎮");
        let roundtrip: OnlineMember = serde_json::from_value(wire).unwrap();
        assert_eq!(roundtrip.display_name(), "Büğra 🎮");
        member.display_name = "   ".into();
        assert_eq!(member.display_name(), "user_123");
    }
}
