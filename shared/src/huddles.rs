//! Voice huddles: a live voice channel split into private sub-calls without
//! anyone reconnecting. See docs/huddles.md for the rules and lifecycle.
use crate::{AccountId, ChannelId, GuildId};
use serde::{Deserialize, Serialize};

/// `POST` a [`HuddleRequest`]; the reply is the channel's [`HuddleState`].
pub const HUDDLES_PATH: &str = "/api/v1/voice/huddles";
/// Huddles are numbered `1..=MAX_HUDDLES`; 0 is the main room. A full room of
/// eight people fits four pairs.
pub const MAX_HUDDLES: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HuddleRequest {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub change: HuddleChange,
}

/// Every change is idempotent, so a repeated or concurrent request (two people
/// pressing the shortcut at once) converges instead of toggling back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum HuddleChange {
    /// Puts someone currently in the channel into a huddle, or 0 for the main room.
    Assign { account_id: AccountId, huddle: u8 },
    /// Splits the room into its huddles (`true`) or brings everyone back.
    Split { active: bool },
    /// Forgets every assignment and brings everyone back.
    Clear {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuddleAssignment {
    pub account_id: AccountId,
    pub huddle: u8,
}

/// The channel's plan after a change: whether it is split, and the huddle of
/// everyone currently in it who is not in the main room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuddleState {
    pub split: bool,
    /// Who last split or regrouped the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_by: Option<AccountId>,
    #[serde(default)]
    pub assignments: Vec<HuddleAssignment>,
}

impl HuddleChange {
    pub fn valid(self) -> bool {
        !matches!(self, Self::Assign { huddle, .. } if huddle > MAX_HUDDLES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_have_a_stable_wire_format() {
        let id = "00000000-0000-0000-0000-000000000001";
        let request: HuddleRequest = serde_json::from_value(json!({
            "guild_id": id, "channel_id": id,
            "change": {"action": "assign", "account_id": id, "huddle": 2}
        }))
        .unwrap();
        assert!(matches!(
            request.change,
            HuddleChange::Assign { huddle: 2, .. }
        ));
        assert_eq!(
            serde_json::to_value(HuddleChange::Split { active: true }).unwrap(),
            json!({"action": "split", "active": true})
        );
        assert_eq!(
            serde_json::to_value(HuddleChange::Clear {}).unwrap(),
            json!({"action": "clear"})
        );
        assert!(serde_json::from_value::<HuddleChange>(json!({"action": "split"})).is_err());
        assert!(
            serde_json::from_value::<HuddleRequest>(json!({
                "guild_id": id, "channel_id": id, "change": {"action": "clear"}, "extra": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn soundboard_plays_stay_inside_a_split_huddle() {
        let mut listener: crate::voice::Participant = serde_json::from_value(json!({
            "account_id": "00000000-0000-0000-0000-000000000001", "username": "l",
            "slot": 0, "muted": false, "deafened": false, "can_speak": true
        }))
        .unwrap();
        // Together: every play is heard, wherever it came from.
        assert!(listener.hears_from(None));
        listener.huddle = 2;
        assert!(listener.hears_from(Some(1)));
        listener.split = true;
        assert!(listener.hears_from(Some(2)));
        assert!(!listener.hears_from(Some(1)));
        assert!(!listener.hears_from(Some(0)));
        assert!(listener.hears_from(None));
        // Old servers and clients omit the new fields.
        let wire = serde_json::to_value(&listener).unwrap();
        assert_eq!(wire["huddle"], 2);
        assert!(wire.get("split_by").is_none());
    }

    #[test]
    fn huddle_numbers_are_bounded() {
        let account_id = AccountId::from_uuid(uuid::Uuid::nil());
        assert!(
            HuddleChange::Assign {
                account_id,
                huddle: 0
            }
            .valid()
        );
        assert!(
            HuddleChange::Assign {
                account_id,
                huddle: MAX_HUDDLES
            }
            .valid()
        );
        assert!(
            !HuddleChange::Assign {
                account_id,
                huddle: MAX_HUDDLES + 1
            }
            .valid()
        );
        assert!(HuddleChange::Split { active: false }.valid());
    }
}
