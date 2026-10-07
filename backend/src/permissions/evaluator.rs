//! The backend is authoritative. No cached grants survive a database mutation.
use thiscord_shared::{AccountId, ChannelId, permissions::*};

pub fn effective(
    state: &GuildState,
    account: AccountId,
    channel: Option<ChannelId>,
) -> Permissions {
    let Some(member) = state.members.iter().find(|m| m.account_id == account) else {
        return Permissions::new();
    };
    if channel.is_some_and(|id| !state.channels.iter().any(|c| c.id == id)) {
        return Permissions::new();
    }
    let mut permissions: Permissions = state
        .roles
        .iter()
        .filter(|r| r.everyone || member.roles.contains(&r.id))
        .flat_map(|r| r.permissions.iter().copied())
        .collect();
    let timed_out = member
        .timeout_until
        .is_some_and(|until| until > chrono::Utc::now())
        && state.guild.owner != account;
    if !timed_out
        && (state.guild.owner == account || permissions.contains(&Permission::Administrator))
    {
        return Permission::ALL.into_iter().collect();
    }
    if let Some(channel) = channel {
        let overrides: Vec<_> = state
            .overrides
            .iter()
            .filter(|o| o.channel_id == channel)
            .collect();
        let everyone = state.roles.iter().find(|r| r.everyone).map(|r| r.id);
        let apply = |permissions: &mut Permissions, overrides: Vec<&ChannelOverride>| {
            for o in &overrides {
                permissions.retain(|p| !o.deny.contains(p));
            }
            for o in overrides {
                permissions.extend(&o.allow);
            }
        };
        apply(
            &mut permissions,
            overrides
                .iter()
                .copied()
                .filter(|o| matches!(o.target, OverrideTarget::Role(id) if Some(id)==everyone))
                .collect(),
        );
        apply(&mut permissions, overrides.iter().copied().filter(|o| matches!(o.target, OverrideTarget::Role(id) if Some(id)!=everyone && member.roles.contains(&id))).collect());
        apply(
            &mut permissions,
            overrides
                .into_iter()
                .filter(|o| o.target == OverrideTarget::Member(account))
                .collect(),
        );
        if !permissions.contains(&Permission::ViewChannel) {
            permissions.retain(|p| {
                matches!(
                    p,
                    Permission::ManageGuild
                        | Permission::ManageRoles
                        | Permission::ManageChannels
                        | Permission::ManageInvites
                        | Permission::KickMembers
                        | Permission::BanMembers
                        | Permission::ModerateMembers
                )
            });
        }
        if !permissions.contains(&Permission::JoinVoice) {
            permissions.remove(&Permission::Speak);
        }
    }
    // Apply last so role/member allowances cannot bypass a timeout. Reading stays available.
    if timed_out {
        permissions.retain(|p| matches!(p, Permission::ViewChannel | Permission::ReadHistory));
    }
    permissions
}

pub fn rank(state: &GuildState, account: AccountId) -> i32 {
    if state.guild.owner == account {
        return i32::MAX;
    }
    state
        .members
        .iter()
        .find(|m| m.account_id == account)
        .map(|m| {
            state
                .roles
                .iter()
                .filter(|r| m.roles.contains(&r.id))
                .map(|r| r.position)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(-1)
}
