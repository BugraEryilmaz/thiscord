//! Browser-independent member list and custom status helpers.
use thiscord_shared::{
    AccountId,
    account::{CustomStatus, MAX_STATUS_SECONDS},
    chat::{GuildMember, MemberActivity, MemberRole},
};

/// Device-local member panel visibility (wide layouts). Contains no account data.
pub const PANEL_KEY: &str = "thiscord.member_panel.v1";

/// A member list group heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Section {
    /// Online members whose highest-ranked role is this one.
    Role(MemberRole),
    /// Online members without roles (Everyone only).
    Online,
    Offline,
}

impl Section {
    /// Stable identity for keyed rendering and element IDs; a renamed or
    /// re-ranked role keeps its key.
    pub fn key(&self) -> String {
        match self {
            Self::Role(role) => format!("role-{}", role.id),
            Self::Online => "online".into(),
            Self::Offline => "offline".into(),
        }
    }
}

/// Groups members once each: online members under their highest-ranked role
/// (highest rank first, ties by name then ID), then online members without
/// roles, then everyone offline. Members keep the server's display-name order.
pub fn sections(members: &[GuildMember]) -> Vec<(Section, Vec<&GuildMember>)> {
    let mut roles: Vec<(&MemberRole, Vec<&GuildMember>)> = Vec::new();
    let (mut online, mut offline) = (Vec::new(), Vec::new());
    for member in members {
        match (member.online, &member.role) {
            (false, _) => offline.push(member),
            (true, None) => online.push(member),
            (true, Some(role)) => match roles.iter_mut().find(|(r, _)| r.id == role.id) {
                Some((_, group)) => group.push(member),
                None => roles.push((role, vec![member])),
            },
        }
    }
    roles.sort_by(|(a, _), (b, _)| {
        b.position
            .cmp(&a.position)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    roles
        .into_iter()
        .map(|(role, group)| (Section::Role(role.clone()), group))
        .chain([(Section::Online, online), (Section::Offline, offline)])
        .filter(|(_, group)| !group.is_empty())
        .collect()
}

/// Applies a complete `member_presence` set to the last full list.
pub fn apply_presence(
    members: &mut [GuildMember],
    online: &[AccountId],
    activities: &[MemberActivity],
) {
    for member in members {
        member.online = online.contains(&member.account_id);
        member.activity = activities
            .iter()
            .find(|a| a.account_id == member.account_id && member.online)
            .map(|a| a.activity.clone());
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
            activity: None,
            role: None,
            owner: false,
            bot: false,
        }
    }

    fn role(id: u128, name: &str, position: i32) -> MemberRole {
        MemberRole {
            id: format!("00000000-0000-0000-0000-{id:012}").parse().unwrap(),
            name: name.into(),
            position,
        }
    }

    fn names(group: &[&GuildMember]) -> Vec<String> {
        group.iter().map(|m| m.username.clone()).collect()
    }

    #[test]
    fn sections_group_online_members_by_highest_role_once() {
        let agalar = role(90, "agalar", 20);
        let mods = role(91, "mods", 10);
        let tied = role(92, "aaa", 10);
        let mut members: Vec<_> = (1..=7).map(|id| member(id, id != 6)).collect();
        members[0].role = Some(mods.clone());
        members[1].role = Some(agalar.clone());
        members[3].role = Some(agalar.clone());
        members[4].role = Some(tied.clone());
        // Offline role holders are listed under Offline, not their role.
        members[5].role = Some(agalar.clone());
        let sections = sections(&members);
        let summary: Vec<_> = sections
            .iter()
            .map(|(section, group)| (section.key(), names(group)))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    format!("role-{}", agalar.id),
                    vec!["user2".to_owned(), "user4".to_owned()]
                ),
                (format!("role-{}", tied.id), vec!["user5".to_owned()]),
                (format!("role-{}", mods.id), vec!["user1".to_owned()]),
                (
                    "online".to_owned(),
                    vec!["user3".to_owned(), "user7".to_owned()]
                ),
                ("offline".to_owned(), vec!["user6".to_owned()]),
            ]
        );
        assert_eq!(sections[0].0, Section::Role(agalar));
        // Everyone appears exactly once; empty groups are omitted.
        assert_eq!(sections.iter().map(|(_, g)| g.len()).sum::<usize>(), 7);
        let all_offline: Vec<_> = (1..=2).map(|id| member(id, false)).collect();
        assert_eq!(
            super::sections(&all_offline)
                .iter()
                .map(|(s, _)| s.key())
                .collect::<Vec<_>>(),
            ["offline"]
        );
        assert!(super::sections(&[]).is_empty());
    }

    #[test]
    fn grouping_keeps_order_and_presence_replaces_previous_flags() {
        let mut members = vec![member(1, false), member(2, true), member(3, false)];
        let ids = [members[0].account_id, members[2].account_id];
        let playing = |member: &GuildMember| MemberActivity {
            account_id: member.account_id,
            activity: thiscord_shared::account::Activity::playing("Factorio"),
        };
        let activities = [playing(&members[0]), playing(&members[1])];
        apply_presence(&mut members, &ids, &activities);
        assert_eq!(
            members
                .iter()
                .map(|m| (m.online, m.activity.is_some()))
                .collect::<Vec<_>>(),
            // Activities never show on offline members.
            [(true, true), (false, false), (true, false)]
        );
        apply_presence(&mut members, &[], &[]);
        assert!(members.iter().all(|m| !m.online && m.activity.is_none()));
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
