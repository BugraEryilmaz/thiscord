//! Account-local sidebar preferences and presentation-only hierarchy checks.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiscord_shared::{AccountId, ChannelId, permissions::GuildState};

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
