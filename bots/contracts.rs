//! Scripting bots are guild-scoped nonhuman accounts. Credentials are returned once.
use crate::{AccountId, ApiError, ChannelId, GuildId, chat::ChatMessage};
use serde::{Deserialize, Serialize};

pub const BOTS_PATH: &str = "/api/v1/bots";
pub const SOCKET_PATH: &str = "/api/v1/bots/socket";
pub const SOCKET_VERSION: u8 = 1;
pub const MAX_CHANNELS: usize = 32;
pub const MAX_BOTS_PER_GUILD: usize = 10;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotInfo {
    pub account_id: AccountId,
    pub guild_id: GuildId,
    /// Globally unique lowercase account handle used by @mentions.
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotRequest {
    List {
        guild_id: GuildId,
    },
    Create {
        guild_id: GuildId,
        revision: i64,
        name: String,
    },
    Rotate {
        guild_id: GuildId,
        revision: i64,
        account_id: AccountId,
    },
    Delete {
        guild_id: GuildId,
        revision: i64,
        account_id: AccountId,
    },
}

// Deliberately no Debug: Credential contains a bearer secret.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BotResponse {
    Bots {
        bots: Vec<BotInfo>,
        revision: i64,
    },
    Credential {
        bot: BotInfo,
        token: String,
        revision: i64,
    },
    Done {
        revision: i64,
    },
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
    Authenticate {
        token: String,
        channels: Vec<ChannelId>,
    },
    Ping {},
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Ready {
        bot: BotInfo,
    },
    /// New committed human messages only. No history, edit or retry replay.
    /// Delivery is live and best effort; reconnect starts a new subscription.
    Mention {
        message: Box<ChatMessage>,
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
    fn stable_tagged_contract_and_strict_authentication() {
        let value =
            json!({"version":1,"event":{"type":"authenticate","token":"bot_secret","channels":[]}});
        let frame: ClientFrame = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), value);
        let mut invalid = value;
        invalid["event"]["account_id"] = json!("00000000-0000-0000-0000-000000000001");
        assert!(serde_json::from_value::<ClientFrame>(invalid).is_err());
        assert_eq!(
            serde_json::to_value(ServerEvent::Revoked {}).unwrap(),
            json!({"type":"revoked"})
        );
        assert_eq!(
            serde_json::to_value(BotResponse::Done { revision: 4 }).unwrap(),
            json!({"result":"done","revision":4})
        );
    }
}
