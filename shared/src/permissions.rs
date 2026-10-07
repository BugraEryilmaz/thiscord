//! Stable, explicit permission names; unknown permissions fail deserialization.
use crate::{AccountId, ChannelId, GuildId, RoleId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PERMISSIONS_PATH: &str = "/api/v1/permissions";
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Administrator,
    ManageGuild,
    ManageChannels,
    ManageRoles,
    ManageInvites,
    ViewChannel,
    ReadHistory,
    SendMessages,
    EditOwnMessages,
    DeleteOwnMessages,
    ManageMessages,
    KickMembers,
    BanMembers,
    ModerateMembers,
    JoinVoice,
    Speak,
    MuteMembers,
    DeafenMembers,
    MoveMembers,
}
impl Permission {
    pub const ALL: [Self; 19] = [
        Self::Administrator,
        Self::ManageGuild,
        Self::ManageChannels,
        Self::ManageRoles,
        Self::ManageInvites,
        Self::ViewChannel,
        Self::ReadHistory,
        Self::SendMessages,
        Self::EditOwnMessages,
        Self::DeleteOwnMessages,
        Self::ManageMessages,
        Self::KickMembers,
        Self::BanMembers,
        Self::ModerateMembers,
        Self::JoinVoice,
        Self::Speak,
        Self::MuteMembers,
        Self::DeafenMembers,
        Self::MoveMembers,
    ];
}
pub type Permissions = BTreeSet<Permission>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceRole {
    Owner,
    Admin,
    User,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceAccess {
    pub role: InstanceRole,
    pub owner: Option<AccountId>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guild {
    pub id: GuildId,
    pub name: String,
    pub owner: AccountId,
    pub revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub position: i32,
    pub permissions: Permissions,
    pub everyone: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub account_id: AccountId,
    pub username: String,
    pub roles: Vec<RoleId>,
    #[serde(default)]
    pub timeout_until: Option<crate::Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationMember {
    pub account_id: AccountId,
    pub username: String,
    pub timeout_until: Option<crate::Timestamp>,
    pub actions: Permissions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildBan {
    pub account_id: AccountId,
    pub username: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationState {
    pub guild: Guild,
    pub members: Vec<ModerationMember>,
    pub bans: Vec<GuildBan>,
    pub can_unban: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Text,
    Voice,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub kind: ChannelKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum OverrideTarget {
    Role(RoleId),
    Member(AccountId),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelOverride {
    pub channel_id: ChannelId,
    pub target: OverrideTarget,
    pub allow: Permissions,
    pub deny: Permissions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildState {
    pub guild: Guild,
    pub roles: Vec<Role>,
    pub members: Vec<Member>,
    pub channels: Vec<Channel>,
    pub overrides: Vec<ChannelOverride>,
}
// Contains server passwords; deliberately does not implement Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionRequest {
    Instance {},
    SetInstanceAdmin {
        account_id: AccountId,
        admin: bool,
    },
    TransferInstance {
        account_id: AccountId,
    },
    ListGuilds {},
    CreateGuild {
        name: String,
        #[serde(default)]
        password: Option<String>,
    },
    JoinGuild {
        guild_id: GuildId,
        #[serde(default)]
        password: Option<String>,
    },
    ViewGuild {
        guild_id: GuildId,
    },
    Inspect {
        guild_id: GuildId,
    },
    InspectModeration {
        guild_id: GuildId,
    },
    Change {
        guild_id: GuildId,
        revision: i64,
        change: GuildChange,
    },
    Preview {
        guild_id: GuildId,
        account_id: AccountId,
        channel_id: Option<ChannelId>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuildChange {
    Rename {
        name: String,
    },
    Delete {},
    AddMember {
        username: String,
    },
    RemoveMember {
        account_id: AccountId,
    },
    BanMember {
        account_id: AccountId,
    },
    UnbanMember {
        account_id: AccountId,
    },
    /// None clears the timeout; otherwise 1..=2,419,200 seconds (28 days).
    TimeoutMember {
        account_id: AccountId,
        duration_seconds: Option<u32>,
    },
    DisconnectVoice {
        account_id: AccountId,
    },
    Leave {},
    TransferOwner {
        account_id: AccountId,
    },
    CreateRole {
        name: String,
        position: i32,
        permissions: Permissions,
    },
    EditRole {
        role_id: RoleId,
        name: String,
        position: i32,
        permissions: Permissions,
    },
    DeleteRole {
        role_id: RoleId,
    },
    AssignRole {
        account_id: AccountId,
        role_id: RoleId,
        assigned: bool,
    },
    CreateChannel {
        name: String,
        kind: ChannelKind,
    },
    DeleteChannel {
        channel_id: ChannelId,
    },
    SetOverride {
        channel_id: ChannelId,
        target: OverrideTarget,
        allow: Permissions,
        deny: Permissions,
    },
    DeleteOverride {
        channel_id: ChannelId,
        target: OverrideTarget,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PermissionResponse {
    Moderation {
        state: ModerationState,
    },
    Joined {
        guild: Guild,
    },
    Home {
        home: GuildHome,
    },
    Instance {
        access: InstanceAccess,
    },
    Guilds {
        guilds: Vec<Guild>,
    },
    State {
        state: GuildState,
    },
    Effective {
        permissions: Permissions,
        revision: i64,
    },
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildHome {
    pub guild: Guild,
    pub channels: Vec<Channel>,
    pub can_manage_roles: bool,
    #[serde(default)]
    pub can_manage_channels: bool,
    #[serde(default)]
    pub can_moderate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moderation_commands_and_legacy_members_keep_stable_wire_formats() {
        let account_id: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        for (action, command) in [
            ("ban_member", GuildChange::BanMember { account_id }),
            ("unban_member", GuildChange::UnbanMember { account_id }),
            (
                "disconnect_voice",
                GuildChange::DisconnectVoice { account_id },
            ),
            (
                "timeout_member",
                GuildChange::TimeoutMember {
                    account_id,
                    duration_seconds: Some(600),
                },
            ),
        ] {
            let value = serde_json::to_value(&command).unwrap();
            assert_eq!(value["action"], action);
            assert_eq!(value["account_id"], json!(account_id));
            let parsed: GuildChange = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
        let legacy: Member =
            serde_json::from_value(json!({"account_id":account_id,"username":"legacy","roles":[]}))
                .unwrap();
        assert!(legacy.timeout_until.is_none());
        assert_eq!(
            serde_json::to_value(Permission::ModerateMembers).unwrap(),
            "moderate_members"
        );
        assert!(
            serde_json::from_value::<GuildChange>(
                json!({"action":"timeout_member","account_id":account_id,"duration_seconds":-1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<GuildChange>(
                json!({"action":"ban_member","account_id":account_id,"guild_id":account_id})
            )
            .is_err()
        );
    }
}
