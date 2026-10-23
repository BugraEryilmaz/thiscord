//! Stable, explicit permission names; unknown permissions fail deserialization.
use crate::{AccountId, AuditId, CategoryId, ChannelId, GuildId, InstanceRoleId, InviteId, RoleId};
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
/// Allocation limits are independent of guild permissions. `user` remains the
/// stable wire name for the Free tier. Members include the owner; roles include Everyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleLimits {
    pub owned_guilds: u32,
    pub text_channels: u32,
    pub voice_channels: u32,
    pub members: u32,
    pub roles: u32,
}
impl RoleLimits {
    pub fn valid(self) -> bool {
        (1..=100).contains(&self.owned_guilds)
            && (1..=100).contains(&self.text_channels)
            && (1..=100).contains(&self.voice_channels)
            && (1..=500).contains(&self.members)
            && (1..=100).contains(&self.roles)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRolePolicy {
    pub id: InstanceRoleId,
    pub name: String,
    pub builtin: bool,
    /// Immutable authority level; custom roles may be Admin or User, never Owner.
    pub role: InstanceRole,
    pub limits: RoleLimits,
    pub revision: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildCapacity {
    pub owner_role: InstanceRole,
    pub role_name: String,
    pub builtin: bool,
    pub limits: RoleLimits,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRoleMember {
    pub account_id: AccountId,
    pub username: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceAccess {
    pub role: InstanceRole,
    pub owner: Option<AccountId>,
    #[serde(default)]
    pub policies: Vec<InstanceRolePolicy>,
    #[serde(default)]
    pub owned_guilds: u32,
    #[serde(default)]
    pub policy_id: Option<InstanceRoleId>,
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
    #[serde(default)]
    pub display_name: String,
    pub roles: Vec<RoleId>,
    #[serde(default)]
    pub timeout_until: Option<crate::Timestamp>,
}
impl Member {
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationMember {
    pub account_id: AccountId,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    pub timeout_until: Option<crate::Timestamp>,
    pub actions: Permissions,
    #[serde(default)]
    pub server_muted: bool,
    #[serde(default)]
    pub server_deafened: bool,
}
impl ModerationMember {
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildBan {
    pub account_id: AccountId,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
}
impl GuildBan {
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
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
    #[serde(default)]
    pub category_id: Option<CategoryId>,
    #[serde(default)]
    pub position: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
    pub position: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuildInvite {
    pub id: InviteId,
    pub created_by: Option<AccountId>,
    pub created_at: crate::Timestamp,
    pub expires_at: crate::Timestamp,
    pub max_uses: u32,
    pub uses: u32,
    pub revoked: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: AuditId,
    pub created_at: crate::Timestamp,
    pub actor: Option<AccountId>,
    pub target: Option<AccountId>,
    pub action: String,
    pub reason: String,
    pub details: String,
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
    #[serde(default)]
    pub capacity: Option<GuildCapacity>,
    pub roles: Vec<Role>,
    pub members: Vec<Member>,
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub categories: Vec<Category>,
    pub overrides: Vec<ChannelOverride>,
}
// Contains server passwords; deliberately does not implement Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PermissionRequest {
    AcceptInvite {
        code: String,
    },
    ListInvites {
        guild_id: GuildId,
    },
    CreateInvite {
        guild_id: GuildId,
        revision: i64,
        duration_seconds: u32,
        max_uses: u32,
    },
    RevokeInvite {
        guild_id: GuildId,
        revision: i64,
        invite_id: InviteId,
    },
    InspectOrganization {
        guild_id: GuildId,
    },
    AuditLog {
        guild_id: GuildId,
        #[serde(default)]
        page: crate::pagination::PageRequest,
    },
    Moderate {
        guild_id: GuildId,
        revision: i64,
        change: GuildChange,
        reason: String,
    },
    Instance {},
    ListInstanceRoleMembers {
        role_id: InstanceRoleId,
        #[serde(default)]
        page: crate::pagination::PageRequest,
    },
    SetInstanceRoleLimits {
        role_id: InstanceRoleId,
        name: String,
        revision: i64,
        limits: RoleLimits,
    },
    CreateInstanceRole {
        name: String,
        role: InstanceRole,
        limits: RoleLimits,
    },
    DeleteInstanceRole {
        role_id: InstanceRoleId,
        revision: i64,
    },
    SetInstanceRole {
        account_id: AccountId,
        role_id: InstanceRoleId,
    },
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
    SetServerMute {
        account_id: AccountId,
        muted: bool,
    },
    SetServerDeafen {
        account_id: AccountId,
        deafened: bool,
    },
    MoveVoice {
        account_id: AccountId,
        source_channel_id: ChannelId,
        channel_id: ChannelId,
    },
    EditChannel {
        channel_id: ChannelId,
        name: String,
        category_id: Option<CategoryId>,
        position: i32,
    },
    CreateCategory {
        name: String,
    },
    EditCategory {
        category_id: CategoryId,
        name: String,
        position: i32,
    },
    DeleteCategory {
        category_id: CategoryId,
    },
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
// Invite creation returns a bearer capability once; never derive Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PermissionResponse {
    Invites {
        guild: Guild,
        invites: Vec<GuildInvite>,
    },
    InviteCreated {
        guild: Guild,
        invite: GuildInvite,
        code: String,
    },
    Organization {
        guild: Guild,
        channels: Vec<Channel>,
        categories: Vec<Category>,
    },
    AuditLog {
        page: crate::pagination::Page<AuditEntry>,
    },
    InstanceRoleMembers {
        role_id: InstanceRoleId,
        page: crate::pagination::Page<InstanceRoleMember>,
    },
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
    #[serde(default)]
    pub categories: Vec<Category>,
    #[serde(default)]
    pub can_manage_invites: bool,
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
    fn organization_defaults_and_moderation_reason_are_wire_compatible() {
        let id = "00000000-0000-0000-0000-000000000001";
        let channel: Channel =
            serde_json::from_value(json!({"id":id,"name":"old","kind":"voice"})).unwrap();
        assert!(channel.category_id.is_none());
        assert_eq!(channel.position, 0);
        let command = json!({"action":"moderate","guild_id":id,"revision":3,"reason":"Repeated noise","change":{"action":"set_server_mute","account_id":id,"muted":true}});
        let parsed: PermissionRequest = serde_json::from_value(command.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), command);
        assert!(serde_json::from_value::<PermissionRequest>(json!({"action":"create_invite","guild_id":id,"revision":0,"duration_seconds":3600,"max_uses":-1})).is_err());
    }

    #[test]
    fn extensible_instance_role_commands_are_explicit_and_bounded() {
        let limits = RoleLimits {
            owned_guilds: 1,
            text_channels: 2,
            voice_channels: 2,
            members: 25,
            roles: 10,
        };
        assert!(limits.valid());
        assert!(
            !RoleLimits {
                owned_guilds: 0,
                ..limits
            }
            .valid()
        );
        assert!(
            !RoleLimits {
                voice_channels: 101,
                ..limits
            }
            .valid()
        );
        assert!(
            !RoleLimits {
                members: 501,
                ..limits
            }
            .valid()
        );
        assert!(
            !RoleLimits {
                roles: 101,
                ..limits
            }
            .valid()
        );
        let wire = json!({"action":"create_instance_role","name":"Supporter","role":"user","limits":limits});
        let command: PermissionRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), wire);
        assert_eq!(serde_json::to_value(InstanceRole::User).unwrap(), "user");
        assert_eq!(
            serde_json::to_value(crate::ErrorCode::LimitExceeded).unwrap(),
            "limit_exceeded"
        );
        let mut unknown = wire;
        unknown["administrator"] = json!(true);
        assert!(serde_json::from_value::<PermissionRequest>(unknown).is_err());
    }

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
