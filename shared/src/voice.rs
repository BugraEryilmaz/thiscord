use crate::{AccountId, ApiError, ChannelId, GuildId};
use serde::{Deserialize, Serialize};
pub const VOICE_PATH: &str = "/api/v1/voice";
pub const VOICE_VERSION: u8 = 1;
pub const ROOM_CAPACITY: usize = 8;
pub const SSRC_BASE: u32 = 10_000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Join {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Answer {
        sdp: String,
    },
    State {
        muted: bool,
        deafened: bool,
    },
    Screen {
        active: bool,
        audio: bool,
    },
    Ping {},
    Leave {},
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub account_id: AccountId,
    pub username: String,
    pub slot: usize,
    pub muted: bool,
    pub deafened: bool,
    pub can_speak: bool,
    #[serde(default)]
    pub sharing_screen: bool,
    #[serde(default)]
    pub sharing_audio: bool,
    #[serde(default)]
    pub screen_epoch: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Offer {
        sdp: String,
        slot: usize,
        can_speak: bool,
        #[serde(default)]
        screen_video: bool,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
    },
    Participants {
        members: Vec<Participant>,
    },
    Pong {},
    Revoked {},
    Error {
        error: ApiError,
    },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceStatus {
    pub connected: bool,
    pub channel_id: Option<ChannelId>,
    pub message: String,
    pub participants: Vec<Participant>,
}
// TURN credentials are short-lived secrets; deliberately no Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}
