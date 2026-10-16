//! Account preferences and explicit, authenticated notification subscriptions.
use crate::{
    AccountId, ChannelId, GuildId, Timestamp,
    ids::{CallId, ConversationId},
};
use serde::{Deserialize, Serialize};

pub const PATH: &str = "/api/v1/notifications";
pub const SOCKET_PATH: &str = "/api/v1/notifications/socket";
pub const VERSION: u8 = 1;
pub const MAX_OVERRIDES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    DirectMessage,
    GuildMessage,
    IncomingCall,
    VoiceJoin,
    VoiceLeave,
    Update,
}
impl Kind {
    pub const ALL: [Self; 6] = [
        Self::DirectMessage,
        Self::GuildMessage,
        Self::IncomingCall,
        Self::VoiceJoin,
        Self::VoiceLeave,
        Self::Update,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectMessage => "Direct messages",
            Self::GuildMessage => "Server messages",
            Self::IncomingCall => "Incoming calls",
            Self::VoiceJoin => "Someone joins voice",
            Self::VoiceLeave => "Someone leaves voice",
            Self::Update => "Application updates",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Channel {
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Conversation {
        conversation_id: ConversationId,
    },
    Updates,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    All,
    Mentions,
    Off,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub mode: Mode,
    pub sound: bool,
    pub banner: bool,
    pub volume: u8,
}
impl Delivery {
    pub fn default_for(kind: Kind) -> Self {
        Self {
            mode: Mode::All,
            sound: kind != Kind::Update,
            banner: matches!(
                kind,
                Kind::DirectMessage | Kind::IncomingCall | Kind::Update
            ),
            volume: 70,
        }
    }
    pub fn accepts(self, mentioned: bool) -> bool {
        (self.sound || self.banner)
            && match self.mode {
                Mode::All => true,
                Mode::Mentions => mentioned,
                Mode::Off => false,
            }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mute {
    None,
    Until { at: Timestamp },
    Forever,
}
impl Mute {
    pub fn active(self, now: Timestamp) -> bool {
        match self {
            Self::None => false,
            Self::Until { at } => at > now,
            Self::Forever => true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Snooze {
    Resume,
    Minutes15,
    Hour1,
    Day1,
    Month1,
    Forever,
}
impl Snooze {
    pub const OPTIONS: [(Self, &'static str); 6] = [
        (Self::Resume, "Unmute"),
        (Self::Minutes15, "15 minutes"),
        (Self::Hour1, "1 hour"),
        (Self::Day1, "1 day"),
        (Self::Month1, "1 month (30 days)"),
        (Self::Forever, "Until I unmute"),
    ];
    pub fn mute(self, now: Timestamp) -> Mute {
        let seconds = match self {
            Self::Resume => return Mute::None,
            Self::Forever => return Mute::Forever,
            Self::Minutes15 => 900,
            Self::Hour1 => 3600,
            Self::Day1 => 86400,
            Self::Month1 => 30 * 86400,
        };
        Mute::Until {
            at: now + chrono::Duration::seconds(seconds),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: Kind,
    pub delivery: Delivery,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub target: Target,
    pub delivery: Option<Delivery>,
    pub mute: Mute,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub revision: i64,
    pub defaults: Vec<Rule>,
    pub subscriptions: Vec<Subscription>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            revision: 0,
            defaults: Kind::ALL
                .into_iter()
                .map(|kind| Rule {
                    kind,
                    delivery: Delivery::default_for(kind),
                })
                .collect(),
            subscriptions: vec![],
        }
    }
}
impl Preferences {
    pub fn delivery(&self, target: Target, kind: Kind, now: Timestamp) -> Option<Delivery> {
        let sub = self.subscriptions.iter().find(|s| s.target == target);
        if sub.is_some_and(|s| s.mute.active(now)) {
            return None;
        }
        Some(sub.and_then(|s| s.delivery).unwrap_or_else(|| {
            self.defaults
                .iter()
                .find(|r| r.kind == kind)
                .map_or_else(|| Delivery::default_for(kind), |r| r.delivery)
        }))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Get,
    SetDefault {
        revision: i64,
        kind: Kind,
        delivery: Delivery,
    },
    SetSubscription {
        revision: i64,
        target: Target,
        delivery: Option<Delivery>,
    },
    Snooze {
        target: Target,
        duration: Snooze,
    },
    RemoveSubscription {
        revision: i64,
        target: Target,
    },
    PublishUpdate {
        version: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub target: Target,
    pub kind: Kind,
    pub title: String,
    pub delivery: Delivery,
    pub actor: Option<AccountId>,
    pub call_id: Option<CallId>,
    pub version: Option<String>,
}
// Never Debug: the connect event contains a bearer token.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Connect { token: String, kinds: Vec<Kind> },
    Subscribe { kinds: Vec<Kind> },
    Ping {},
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Ready { preferences: Preferences },
    // Authoritative presentation state, independent of sound/banner preferences.
    Calls { ringing: Vec<CallId> },
    Alert { alert: Alert },
    Pong {},
    Revoked {},
}
#[derive(Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BannerAction {
    Open,
    Accept,
    Decline,
    Snooze { duration: Snooze },
    Dismiss,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct BannerInteraction {
    pub generation: u32,
    pub alert: Alert,
    pub action: BannerAction,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct BannerSnapshot {
    pub revision: u64,
    pub alerts: Vec<Alert>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_snooze_boundaries() {
        let now: Timestamp = "2026-10-16T00:00:00Z".parse().unwrap();
        assert!(Delivery::default_for(Kind::DirectMessage).banner);
        assert!(Delivery::default_for(Kind::IncomingCall).sound);
        assert!(!Delivery::default_for(Kind::GuildMessage).banner);
        let mute = Snooze::Minutes15.mute(now);
        assert!(mute.active(now + chrono::Duration::seconds(899)));
        assert!(!mute.active(now + chrono::Duration::seconds(900)));
        let mut p = Preferences::default();
        p.subscriptions.push(Subscription {
            target: Target::Updates,
            delivery: Some(Delivery {
                mode: Mode::Off,
                ..Delivery::default_for(Kind::Update)
            }),
            mute,
        });
        assert!(p.delivery(Target::Updates, Kind::Update, now).is_none());
        assert!(
            !p.delivery(
                Target::Updates,
                Kind::Update,
                now + chrono::Duration::hours(1)
            )
            .unwrap()
            .accepts(true)
        );
        assert!(
            Snooze::Forever
                .mute(now)
                .active(now + chrono::Duration::days(10000))
        );
    }
    #[test]
    fn mentions_are_explicit_and_wire_is_stable() {
        let delivery = Delivery {
            mode: Mode::Mentions,
            ..Delivery::default_for(Kind::GuildMessage)
        };
        assert!(!delivery.accepts(false));
        assert!(delivery.accepts(true));
        assert_eq!(
            serde_json::to_value(Snooze::Minutes15).unwrap(),
            "minutes15"
        );
        assert!(serde_json::from_str::<Request>(r#"{"action":"snooze","target":{"type":"updates"},"duration":"forever","unexpected":1}"#).is_err());
    }
}
