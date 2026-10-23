//! Account-local sidebar preferences and presentation-only hierarchy checks.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiscord_shared::{AccountId, ChannelId, permissions::GuildState};

/// Presentation choices for a person's menu. The backend still authorizes every command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendAction {
    Add,
    Accept,
    Decline,
    Cancel,
    Unfriend,
}
impl FriendAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "Add friend",
            Self::Accept => "Accept friend request",
            Self::Decline => "Decline friend request",
            Self::Cancel => "Cancel friend request",
            Self::Unfriend => "Unfriend…",
        }
    }
    pub fn request(self, account_id: AccountId) -> thiscord_shared::friends::Request {
        use thiscord_shared::friends::Request;
        match self {
            Self::Add => Request::Add { account_id },
            Self::Accept => Request::Accept { account_id },
            Self::Decline | Self::Cancel | Self::Unfriend => Request::Remove { account_id },
        }
    }
}
pub fn friend_actions(
    own: bool,
    relationship: Option<thiscord_shared::friends::Relationship>,
) -> &'static [FriendAction] {
    use thiscord_shared::friends::Relationship;
    if own {
        return &[];
    }
    match relationship {
        None => &[FriendAction::Add],
        Some(Relationship::Friend) => &[FriendAction::Unfriend],
        Some(Relationship::Incoming) => &[FriendAction::Accept, FriendAction::Decline],
        Some(Relationship::Outgoing) => &[FriendAction::Cancel],
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub pinned: BTreeSet<ChannelId>,
    pub muted: BTreeSet<ChannelId>,
}
impl Preferences {
    pub fn toggle(&mut self, channel: ChannelId, pin: bool) {
        let set = if pin {
            &mut self.pinned
        } else {
            &mut self.muted
        };
        if !set.remove(&channel) {
            set.insert(channel);
        }
    }
}

/// Called only after ViewGuild grants ManageRoles. Writes are always authorized
/// again by the backend, including role grants and channel overrides.
pub fn lower_member(state: &GuildState, actor: AccountId, target: AccountId) -> bool {
    if actor == target || target == state.guild.owner {
        return false;
    }
    let rank = |id| {
        state.members.iter().find(|m| m.account_id == id).map(|m| {
            state
                .roles
                .iter()
                .filter(|r| m.roles.contains(&r.id))
                .map(|r| r.position)
                .max()
                .unwrap_or(0)
        })
    };
    match (rank(actor), rank(target)) {
        (Some(a), Some(b)) => actor == state.guild.owner || a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn friend_menu_preserves_consent_and_never_offers_self_actions() {
        use thiscord_shared::friends::{Relationship, Request};
        assert_eq!(friend_actions(false, None), &[FriendAction::Add]);
        assert_eq!(
            friend_actions(false, Some(Relationship::Friend)),
            &[FriendAction::Unfriend]
        );
        assert_eq!(
            friend_actions(false, Some(Relationship::Incoming)),
            &[FriendAction::Accept, FriendAction::Decline]
        );
        assert_eq!(
            friend_actions(false, Some(Relationship::Outgoing)),
            &[FriendAction::Cancel]
        );
        for relationship in [
            None,
            Some(Relationship::Friend),
            Some(Relationship::Incoming),
            Some(Relationship::Outgoing),
        ] {
            assert!(friend_actions(true, relationship).is_empty());
        }
        let target: AccountId = id(7);
        assert!(
            matches!(FriendAction::Add.request(target), Request::Add {account_id} if account_id==target)
        );
        assert!(
            matches!(FriendAction::Accept.request(target), Request::Accept {account_id} if account_id==target)
        );
        for action in [
            FriendAction::Decline,
            FriendAction::Cancel,
            FriendAction::Unfriend,
        ] {
            assert!(
                matches!(action.request(target), Request::Remove {account_id} if account_id==target)
            );
        }
    }
    use thiscord_shared::permissions::*;
    fn id<T: std::str::FromStr>(n: u8) -> T
    where
        T::Err: std::fmt::Debug,
    {
        format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()
    }
    #[test]
    fn preferences_round_trip_and_toggle_independently() {
        let mut p = Preferences::default();
        p.toggle(id(1), true);
        p.toggle(id(2), false);
        let mut p: Preferences = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(p.pinned.contains(&id(1)));
        assert!(p.muted.contains(&id(2)));
        p.toggle(id(1), true);
        assert!(p.pinned.is_empty());
        assert!(p.muted.contains(&id(2)));
    }
    #[test]
    fn role_menu_respects_membership_owner_self_and_rank() {
        let state = GuildState {
            capacity: None,
            guild: Guild {
                id: id(9),
                name: String::new(),
                owner: id(1),
                revision: 1,
            },
            roles: vec![Role {
                id: id(8),
                name: String::new(),
                position: 10,
                permissions: Default::default(),
                everyone: false,
            }],
            members: (1..=4)
                .map(|n| Member {
                    account_id: id(n),
                    username: String::new(),
                    display_name: String::new(),
                    roles: if n == 2 || n == 3 {
                        vec![id(8)]
                    } else {
                        vec![]
                    },
                    timeout_until: None,
                })
                .collect(),
            channels: vec![],
            overrides: vec![],
        };
        assert!(lower_member(&state, id(1), id(2)));
        assert!(lower_member(&state, id(2), id(4)));
        for (a, b) in [(2, 1), (2, 2), (2, 3), (4, 2), (1, 5), (5, 4)] {
            assert!(!lower_member(&state, id(a), id(b)));
        }
    }
}
