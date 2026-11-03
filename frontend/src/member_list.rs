//! Browser-independent member list and custom status helpers.
use thiscord_shared::{
    AccountId,
    account::{CustomStatus, MAX_STATUS_SECONDS},
    chat::GuildMember,
};

/// Device-local member panel visibility (wide layouts). Contains no account data.
pub const PANEL_KEY: &str = "thiscord.member_panel.v1";

/// Online members first, then offline, each keeping the server's display-name order.
pub fn grouped(members: &[GuildMember]) -> (Vec<&GuildMember>, Vec<&GuildMember>) {
    members.iter().partition(|member| member.online)
}

/// Applies a complete `member_presence` set to the last full list.
pub fn apply_presence(members: &mut [GuildMember], online: &[AccountId]) {
    for member in members {
        member.online = online.contains(&member.account_id);
    }
}

/// Single-line plain text for a status ("🎮 Raiding"); never markup.
pub fn status_text(status: &CustomStatus) -> String {
    match (status.emoji.as_deref(), status.text.trim()) {
        (Some(emoji), "") => emoji.to_owned(),
        (Some(emoji), text) => format!("{emoji} {text}"),
        (None, text) => text.to_owned(),
    }
}

/// Expiry presets offered by the status dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusExpiry {
    Never,
    Minutes30,
    Hour1,
    Hours4,
    Today,
}

impl StatusExpiry {
    pub const ALL: [Self; 5] = [
        Self::Never,
        Self::Minutes30,
        Self::Hour1,
        Self::Hours4,
        Self::Today,
    ];

    /// English UI catalog key.
    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "Don't clear",
            Self::Minutes30 => "30 minutes",
            Self::Hour1 => "1 hour",
            Self::Hours4 => "4 hours",
            Self::Today => "Today",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Minutes30 => "30m",
            Self::Hour1 => "1h",
            Self::Hours4 => "4h",
            Self::Today => "today",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|preset| preset.key() == key)
            .unwrap_or(Self::Never)
    }

    /// Lifetime in seconds for the wire request. "Today" ends at the device's
    /// local midnight, `until_midnight` seconds away; the server clock applies it.
    pub fn seconds(self, until_midnight: f64) -> Option<u32> {
        match self {
            Self::Never => None,
            Self::Minutes30 => Some(30 * 60),
            Self::Hour1 => Some(60 * 60),
            Self::Hours4 => Some(4 * 60 * 60),
            Self::Today => Some(
                until_midnight
                    .ceil()
                    .clamp(1.0, f64::from(MAX_STATUS_SECONDS)) as u32,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u128, online: bool) -> GuildMember {
        GuildMember {
            account_id: format!("00000000-0000-0000-0000-{id:012}").parse().unwrap(),
            username: format!("user{id}"),
            display_name: String::new(),
            avatar_id: None,
            online,
            custom_status: None,
            owner: false,
            bot: false,
        }
    }

    #[test]
    fn grouping_keeps_order_and_presence_replaces_previous_flags() {
        let mut members = vec![member(1, false), member(2, true), member(3, false)];
        let (online, offline) = grouped(&members);
        assert_eq!(online.len(), 1);
        assert_eq!(
            offline
                .iter()
                .map(|m| m.username.as_str())
                .collect::<Vec<_>>(),
            ["user1", "user3"]
        );
        let ids = [members[0].account_id, members[2].account_id];
        apply_presence(&mut members, &ids);
        assert_eq!(
            members.iter().map(|m| m.online).collect::<Vec<_>>(),
            [true, false, true]
        );
        apply_presence(&mut members, &[]);
        assert!(members.iter().all(|m| !m.online));
    }

    #[test]
    fn status_text_is_plain_and_handles_missing_parts() {
        let status = |emoji: Option<&str>, text: &str| CustomStatus {
            emoji: emoji.map(Into::into),
            text: text.into(),
            expires_at: None,
        };
        assert_eq!(status_text(&status(Some("🎮"), "Raiding")), "🎮 Raiding");
        assert_eq!(status_text(&status(Some("🎮"), "")), "🎮");
        assert_eq!(status_text(&status(None, "<b>bold</b>")), "<b>bold</b>");
    }

    #[test]
    fn expiry_presets_are_bounded_and_round_trip() {
        assert_eq!(StatusExpiry::Never.seconds(10.0), None);
        assert_eq!(StatusExpiry::Minutes30.seconds(10.0), Some(1800));
        assert_eq!(StatusExpiry::Hours4.seconds(10.0), Some(14_400));
        assert_eq!(StatusExpiry::Today.seconds(3600.2), Some(3601));
        // Just before midnight still sends a valid, non-zero lifetime.
        assert_eq!(StatusExpiry::Today.seconds(0.0), Some(1));
        assert_eq!(StatusExpiry::Today.seconds(1e12), Some(MAX_STATUS_SECONDS));
        for preset in StatusExpiry::ALL {
            assert_eq!(StatusExpiry::from_key(preset.key()), preset);
        }
        assert_eq!(StatusExpiry::from_key("bogus"), StatusExpiry::Never);
    }
}
