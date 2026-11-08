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
/// Member list bound; equal to the enforced per-guild member capacity ceiling.
pub const MAX_MEMBER_LIST: usize = 500;

/// One entry of a guild's member list, sorted by display name then username.
/// `online` is account-wide presence. Presence and custom status are omitted
/// across a block in either direction; Do Not Disturb is never disclosed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuildMember {
    pub account_id: AccountId,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<AvatarId>,
    #[serde(default)]
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<crate::account::CustomStatus>,
    /// Live activity of an online member (see [`crate::account::Activity`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::account::Activity>,
    /// The member's highest-ranked assigned role (never Everyone). Absent
    /// without roles and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MemberRole>,
    /// Highest-ranked assigned role shown separately ("hoisted"): the list
    /// section of an online member. Absent without one and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hoisted_role: Option<MemberRole>,
    /// Name colour: the colour of the highest-ranked coloured assigned role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<crate::permissions::RoleColor>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
}
/// Display data of one of a member's roles: name, rank and colour only, never
/// its permissions or the member's other roles. Higher `position` ranks
/// higher; ties order by name, then ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemberRole {
    pub id: crate::RoleId,
    pub name: String,
    pub position: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<crate::permissions::RoleColor>,
}
/// One online member's live activity in a `member_presence` update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberActivity {
    pub account_id: AccountId,
    pub activity: crate::account::Activity,
}
impl GuildMember {
    /// Profile label, falling back to the username.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

/// Public profile of a fellow guild member, read on demand for profile cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberProfile {
    pub account_id: AccountId,
    pub username: String,
    /// The account-wide profile name, unlike guild member lists and messages.
    pub display_name: String,
    /// Nickname in the requested guild, shown instead of the profile name
    /// there. Absent when unset and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default)]
    pub bio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<AvatarId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<crate::account::CustomStatus>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
}

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
    /// Present on voice messages (validated Ogg Opus). Older clients ignore it
    /// and offer the file as an ordinary audio download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceMetadata>,
    /// Hidden behind a "Spoiler" cover until the viewer reveals it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spoiler: bool,
}

/// Server-verified duration plus the uploader's compact amplitude outline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceMetadata {
    pub duration_ms: u32,
    /// URL-safe base64 of [`crate::voice_message::WAVEFORM_BARS`] bytes (0-255).
    pub waveform: String,
}

/// Raw binary POST query. The server classifies content independently of this hint.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentUpload {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub filename: String,
    pub content_type: String,
    /// Validate the body as an Ogg Opus voice message (length and size bounds).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub voice: bool,
    /// Voice messages only: [`VoiceMetadata::waveform`] computed while recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waveform: Option<String>,
}

/// Previous versions kept per edited message (guild and private). Older
/// versions are discarded; deleting the message deletes all of them.
pub const MAX_EDIT_HISTORY: usize = 20;

/// One replaced version of a message's text.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageVersion {
    pub content: String,
    /// When this version became current (the send or an earlier edit).
    pub created_at: Timestamp,
    /// When an edit replaced it.
    pub replaced_at: Timestamp,
}

/// Previous versions, newest first, at most [`MAX_EDIT_HISTORY`]. The current
/// version is the message itself. Edits made before history was kept are absent.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditHistory {
    pub message_id: MessageId,
    pub versions: Vec<MessageVersion>,
}

/// Fixed-width stand-in for spoiler text in plain-text previews.
pub const SPOILER_PLACEHOLDER: &str = "\u{2592}\u{2592}\u{2592}\u{2592}\u{2592}";

/// Replaces everything from the first to the last `||` with
/// [`SPOILER_PLACEHOLDER`] when the text may contain a spoiler. Used for
/// plain-text previews (replies, announcements, banners). This hides at least
/// everything the Markdown renderer hides, whose delimiters are literal `||`.
pub fn mask_spoilers(text: &str) -> std::borrow::Cow<'_, str> {
    match (text.find("||"), text.rfind("||")) {
        (Some(first), Some(last)) if last >= first + 2 => std::borrow::Cow::Owned(format!(
            "{}{SPOILER_PLACEHOLDER}{}",
            &text[..first],
            &text[last + 2..]
        )),
        _ => std::borrow::Cow::Borrowed(text),
    }
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
/// Unread `count` and `mentions` are exact below this value; a value equal to it
/// means at least this many (display as "999+"). Older servers send exact counts.
pub const UNREAD_COUNT_LIMIT: i64 = 1000;
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
        /// Attachments among `attachment_ids` to publish as spoilers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        spoilers: Vec<AttachmentId>,
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
    /// Requires current membership of both the requester and the target.
    MemberProfile {
        guild_id: GuildId,
        account_id: AccountId,
    },
    /// Previous versions of an undeleted message; same access as reading it.
    EditHistory {
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
    MemberProfile { profile: MemberProfile },
    EditHistory { history: EditHistory },
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
        /// Opt in to `guild_updated` hints so older clients never receive them.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        guild_updates: bool,
        /// The client's [`crate::permissions::PERMISSION_SET`]; newer names are
        /// removed from `subscribed` permissions. Omitted means set 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_set: Option<u8>,
    },
    Subscribe {
        subscription: u64,
        guild_id: Option<GuildId>,
        channel_id: Option<ChannelId>,
        /// Opt in so older clients never receive an unknown event variant.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        voice_roster: bool,
        /// Opt in to the guild member list (`members` / `member_presence` updates).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        members: bool,
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
    /// Complete member list, replacing any previous one for the subscription.
    Members {
        members: Vec<GuildMember>,
    },
    /// Complete set of members currently shown online; applies to the last list.
    MemberPresence {
        online: Vec<AccountId>,
        /// Complete set of live activities, all for members in `online`. Older
        /// servers omit it (no activities); older clients ignore it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        activities: Vec<MemberActivity>,
    },
    Pong {},
    Revoked {},
    Error {
        error: ApiError,
    },
    /// Presentation metadata (name/icon) of a guild the account belongs to changed.
    /// A hint only: clients re-read guilds over authorized HTTP. Sent only to
    /// sockets that opted in with `guild_updates` on `connect`.
    GuildUpdated {
        guild_id: GuildId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn voice_spoiler_and_edit_history_fields_are_additive() {
        let id = "00000000-0000-0000-0000-000000000001";
        // Legacy metadata (stored rows, older servers) decodes without the new fields.
        let legacy = json!({"id":id,"filename":"a.png","content_type":"image/png","size_bytes":3});
        let attachment: ChatAttachment = serde_json::from_value(legacy.clone()).unwrap();
        assert!(attachment.voice.is_none() && !attachment.spoiler);
        assert_eq!(serde_json::to_value(&attachment).unwrap(), legacy);
        let voice = ChatAttachment {
            voice: Some(VoiceMetadata {
                duration_ms: 1500,
                waveform: "AA".into(),
            }),
            spoiler: true,
            ..attachment
        };
        let wire = serde_json::to_value(&voice).unwrap();
        assert_eq!(wire["voice"]["duration_ms"], 1500);
        assert_eq!(wire["spoiler"], true);
        // Released 0.1.61 clients decode the attachment as an ordinary file:
        // their struct has exactly the legacy fields and ignores the rest.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Released {
            id: AttachmentId,
            filename: String,
            content_type: String,
            size_bytes: u64,
        }
        let old: Released = serde_json::from_value(wire).unwrap();
        assert_eq!(old.content_type, "image/png");
        // Old send payloads omit spoilers and new ones omit an empty list.
        let send: ChatRequest = serde_json::from_value(json!({"action":"send","guild_id":id,"channel_id":id,"client_id":id,"content":"x","attachment_ids":[id]})).unwrap();
        assert!(matches!(&send, ChatRequest::Send { spoilers, .. } if spoilers.is_empty()));
        assert!(
            serde_json::to_value(&send)
                .unwrap()
                .get("spoilers")
                .is_none()
        );
        let upload: AttachmentUpload = serde_json::from_value(
            json!({"guild_id":id,"channel_id":id,"filename":"f","content_type":"x"}),
        )
        .unwrap();
        assert!(!upload.voice && upload.waveform.is_none());
        let wire = json!({"action":"edit_history","guild_id":id,"channel_id":id,"message_id":id});
        let request: ChatRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), wire);
        let response = json!({"result":"edit_history","history":{"message_id":id,"versions":[{"content":"old","created_at":"2026-01-01T00:00:00Z","replaced_at":"2026-01-01T00:01:00Z"}]}});
        let parsed: ChatResponse = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), response);
    }

    #[test]
    fn spoiler_masking_covers_every_possible_spoiler() {
        let hidden = SPOILER_PLACEHOLDER;
        assert_eq!(mask_spoilers("plain | text"), "plain | text");
        assert_eq!(mask_spoilers("a ||b|| c"), format!("a {hidden} c"));
        // Unpaired delimiters stay visible; text between separate spoilers is hidden.
        assert_eq!(mask_spoilers("||open"), "||open");
        assert_eq!(mask_spoilers("|||"), "|||");
        assert_eq!(mask_spoilers("x ||a|| y ||b|| z"), format!("x {hidden} z"));
        assert_eq!(mask_spoilers("||a\n\nb|| c ||d||"), hidden);
        assert_eq!(
            mask_spoilers("\u{fc}n ||\u{e7}\u{f6}de|| \u{11f}"),
            format!("\u{fc}n {hidden} \u{11f}")
        );
    }

    #[test]
    fn guild_update_hints_are_opt_in() {
        let legacy = json!({"type":"connect","token":"t"});
        let ClientEvent::Connect { guild_updates, .. } =
            serde_json::from_value(legacy.clone()).unwrap()
        else {
            panic!()
        };
        assert!(!guild_updates);
        let event = ClientEvent::Connect {
            token: "t".into(),
            guild_updates: false,
            permission_set: None,
        };
        assert_eq!(serde_json::to_value(event).unwrap(), legacy);
        let opted = json!({"type":"connect","token":"t","guild_updates":true});
        assert!(matches!(
            serde_json::from_value(opted).unwrap(),
            ClientEvent::Connect {
                guild_updates: true,
                ..
            }
        ));
        let id = "00000000-0000-0000-0000-000000000001";
        let hint = json!({"type":"guild_updated","guild_id":id});
        let parsed: ServerEvent = serde_json::from_value(hint.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), hint);
    }

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
    fn member_list_contracts_are_opt_in_and_additive() {
        let id = "00000000-0000-0000-0000-000000000001";
        let legacy: ClientEvent = serde_json::from_value(
            json!({"type":"subscribe","subscription":1,"guild_id":id,"channel_id":null}),
        )
        .unwrap();
        assert!(matches!(
            legacy,
            ClientEvent::Subscribe { members: false, .. }
        ));
        let opted = ClientEvent::Subscribe {
            subscription: 2,
            guild_id: None,
            channel_id: None,
            voice_roster: false,
            members: true,
        };
        assert_eq!(serde_json::to_value(&opted).unwrap()["members"], true);
        // Minimal entries decode; offline members without status stay compact.
        let member: GuildMember =
            serde_json::from_value(json!({"account_id":id,"username":"user_1"})).unwrap();
        assert_eq!(member.display_name(), "user_1");
        assert!(!member.online && !member.owner && !member.bot && member.role.is_none());
        let ranked: GuildMember = serde_json::from_value(json!({"account_id":id,"username":"u",
            "role":{"id":id,"name":"agalar","position":10}}))
        .unwrap();
        assert_eq!(
            serde_json::to_value(ranked.role).unwrap(),
            json!({"id":id,"name":"agalar","position":10})
        );
        assert_eq!(
            serde_json::to_value(&member).unwrap(),
            json!({"account_id":id,"username":"user_1","display_name":"","online":false})
        );
        let event = ServerEvent::Update {
            subscription: 2,
            event: Box::new(ServerEvent::MemberPresence {
                online: vec![member.account_id],
                activities: vec![],
            }),
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["event"]["type"], "member_presence");
        assert_eq!(wire["event"]["online"][0], id);
        assert!(wire["event"].get("activities").is_none());
        let playing: ServerEvent =
            serde_json::from_value(json!({"type":"member_presence","online":[id],
            "activities":[{"account_id":id,"activity":{"kind":"playing","name":"Factorio"}}]}))
            .unwrap();
        assert!(
            matches!(playing, ServerEvent::MemberPresence { activities, .. }
            if activities.len() == 1 && activities[0].activity.name == "Factorio")
        );
        let request = json!({"action":"member_profile","guild_id":id,"account_id":id});
        let parsed: ChatRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);
        let response = json!({"result":"member_profile","profile":{"account_id":id,"username":"u","display_name":"U","bio":"Hi","custom_status":{"emoji":"🎮","text":"Playing"}}});
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

    #[test]
    fn role_display_and_nickname_fields_are_additive() {
        let id = "00000000-0000-0000-0000-000000000001";
        // A 0.1.61 client ignores the new member-list fields; a new client
        // reads lists from older servers without them.
        let current = json!({"account_id":id,"username":"u","display_name":"Nick","online":false,
            "role":{"id":id,"name":"Mods","position":5,"color":0x2fa85a},
            "hoisted_role":{"id":id,"name":"Mods","position":5,"color":0x2fa85a},
            "color":0x2fa85a});
        let member: GuildMember = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(member.color.unwrap().hex(), "#2fa85a");
        assert_eq!(serde_json::to_value(&member).unwrap(), current);
        let legacy: GuildMember = serde_json::from_value(json!({"account_id":id,"username":"u",
                "role":{"id":id,"name":"Mods","position":5}}))
        .unwrap();
        assert!(legacy.hoisted_role.is_none() && legacy.color.is_none());
        assert!(legacy.role.unwrap().color.is_none());
        assert!(
            serde_json::from_value::<MemberRole>(
                json!({"id":id,"name":"x","position":1,"color":0x1000000})
            )
            .is_err()
        );
        let profile: MemberProfile = serde_json::from_value(
            json!({"account_id":id,"username":"u","display_name":"Profile","nickname":"Nick"}),
        )
        .unwrap();
        assert_eq!(profile.nickname.as_deref(), Some("Nick"));
        let connect = json!({"type":"connect","token":"t","permission_set":2});
        assert!(matches!(
            serde_json::from_value(connect).unwrap(),
            ClientEvent::Connect {
                permission_set: Some(2),
                ..
            }
        ));
    }
}
