//! Stable, explicit permission names; unknown permissions fail deserialization.
use crate::{
    AccountId, AuditId, CategoryId, ChannelId, GuildIconId, GuildId, InstanceRoleId, InviteId,
    RoleId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PERMISSIONS_PATH: &str = "/api/v1/permissions";
/// `POST` sets/removes a guild icon; `GET {GUILD_ICON_PATH}/{icon_id}` serves it.
pub const GUILD_ICON_PATH: &str = "/api/v1/guild-icons";
/// Icons reuse the profile-picture limits and normalization.
pub const MAX_GUILD_ICON_BYTES: usize = crate::account::MAX_AVATAR_BYTES;
pub const MAX_GUILD_ICON_BASE64: usize = crate::account::MAX_AVATAR_BASE64;
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
    /// See and play a guild's soundboard sounds in a voice channel.
    UseSoundboard,
    /// Set or clear one's own nickname in this guild.
    ChangeNickname,
    /// Set or clear lower-ranked members' nicknames (audited).
    ManageNicknames,
}
impl Permission {
    pub const ALL: [Self; 22] = [
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
        Self::UseSoundboard,
        Self::ChangeNickname,
        Self::ManageNicknames,
    ];

    /// The [permission set](PERMISSION_SET) that introduced this name. Released
    /// clients reject unknown names, so servers only send a permission to
    /// clients that declare a set at least this new.
    pub const fn set(self) -> u8 {
        match self {
            Self::ChangeNickname | Self::ManageNicknames => 2,
            _ => 1,
        }
    }
}

/// Newest permission set this build understands. Set 1 is every name known to
/// client 0.1.61; set 2 adds the nickname permissions.
pub const PERMISSION_SET: u8 = 2;
/// Query parameter on [`PERMISSIONS_PATH`] declaring the client's permission set.
/// Omitted means set 1. A query (unlike a header) needs no CORS change and is
/// ignored by older servers.
pub const PERMISSION_SET_QUERY: &str = "permission_set";

/// [`PERMISSIONS_PATH`] declaring this build's [`PERMISSION_SET`].
pub fn permissions_path() -> String {
    format!("{PERMISSIONS_PATH}?{PERMISSION_SET_QUERY}={PERMISSION_SET}")
}

/// Removes names newer than a client's permission set before sending.
pub fn restrict(permissions: &mut Permissions, set: u8) {
    permissions.retain(|p| p.set() <= set);
}

/// Stored names a client of this set cannot see. Its full-replacement writes
/// (role edits, channel overrides) must preserve them.
pub fn hidden(permissions: &Permissions, set: u8) -> impl Iterator<Item = Permission> + '_ {
    permissions.iter().copied().filter(move |p| p.set() > set)
}

/// Opaque sRGB role colour, `0xRRGGBB` as a JSON integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct RoleColor(u32);
impl RoleColor {
    pub const fn new(rgb: u32) -> Option<Self> {
        if rgb <= 0xff_ffff {
            Some(Self(rgb))
        } else {
            None
        }
    }
    pub const fn rgb(self) -> (u8, u8, u8) {
        ((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8)
    }
    /// Lowercase `#rrggbb`.
    pub fn hex(self) -> String {
        format!("#{:06x}", self.0)
    }
}
impl TryFrom<u32> for RoleColor {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value).ok_or("role colour must be 0xRRGGBB")
    }
}
impl From<RoleColor> for u32 {
    fn from(value: RoleColor) -> Self {
        value.0
    }
}

/// How a role is shown: an optional name colour, and whether its members get
/// their own member-list section ("show separately"). Everyone is never hoisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAppearance {
    pub color: Option<RoleColor>,
    pub hoist: bool,
}

/// Longest guild nickname, in Unicode scalar values (the display-name limit).
pub const MAX_NICKNAME_CHARS: usize = crate::account::MAX_DISPLAY_NAME_CHARS;
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
    /// Opaque, replaceable icon image ID. Absent from older servers or when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_id: Option<GuildIconId>,
}

/// Body of `POST {GUILD_ICON_PATH}`. Requires ManageGuild and the current guild
/// revision; `image_base64: null` removes the icon. Responds with the updated [`Guild`].
// Carries up to 2 MiB of image data; Debug would dump it into logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGuildIcon {
    pub guild_id: GuildId,
    pub revision: i64,
    pub image_base64: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub position: i32,
    pub permissions: Permissions,
    pub everyone: bool,
    /// Absent when uncoloured and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<RoleColor>,
    /// Its members get their own member-list section. Absent means false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hoist: bool,
}
impl Role {
    pub fn appearance(&self) -> RoleAppearance {
        RoleAppearance {
            color: self.color,
            hoist: self.hoist,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub account_id: AccountId,
    pub username: String,
    /// The member's name in this guild: their nickname when one is set.
    #[serde(default)]
    pub display_name: String,
    /// The guild nickname, if set. Absent from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
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
    /// None means no expiry.
    pub expires_at: Option<crate::Timestamp>,
    /// Zero means unlimited uses.
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
    /// Joining by server ID requires a password. Absent from older servers.
    #[serde(default)]
    pub password_protected: bool,
}

/// Longest accepted server join password, in bytes.
pub const MAX_GUILD_PASSWORD_BYTES: usize = 1024;

/// A server join password on the wire: a plain JSON string whose `Debug`
/// output is redacted, so logging a [`GuildChange`] never reveals it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GuildPassword(pub String);

impl std::fmt::Debug for GuildPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GuildPassword(<redacted>)")
    }
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
        /// Zero means no expiry; otherwise 60..=2_419_200.
        duration_seconds: u32,
        /// Zero means unlimited; otherwise 1..=500.
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
    /// Requires ManageGuild. A non-empty password (at most
    /// [`MAX_GUILD_PASSWORD_BYTES`]) replaces the join password; null removes it.
    /// Invite links keep working either way, and members are never affected.
    SetPassword {
        password: Option<GuildPassword>,
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
        /// Absent (older clients): no colour, not shown separately.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        appearance: Option<RoleAppearance>,
    },
    EditRole {
        role_id: RoleId,
        name: String,
        position: i32,
        permissions: Permissions,
        /// Absent (older clients) keeps the role's current appearance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        appearance: Option<RoleAppearance>,
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
    /// Sets, or with `null` clears, a guild nickname. Your own needs
    /// ChangeNickname; another member's needs ManageNicknames and a strictly
    /// lower-ranked target, and is audited. Validated like display names.
    SetNickname {
        account_id: AccountId,
        nickname: Option<String>,
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
    pub can_manage_bots: bool,
    #[serde(default)]
    pub can_manage_channels: bool,
    #[serde(default)]
    pub can_moderate: bool,
    /// The viewer may set their own nickname here. Absent from older servers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_change_nickname: bool,
}

impl PermissionResponse {
    /// Removes permission names newer than the client's set (see [`Permission::set`]).
    pub fn restrict(&mut self, set: u8) {
        match self {
            Self::State { state } => {
                for role in &mut state.roles {
                    restrict(&mut role.permissions, set);
                }
                for o in &mut state.overrides {
                    restrict(&mut o.allow, set);
                    restrict(&mut o.deny, set);
                }
            }
            Self::Effective { permissions, .. } => restrict(permissions, set),
            Self::Moderation { state } => {
                for member in &mut state.members {
                    restrict(&mut member.actions, set);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn guild_icon_is_optional_and_additive() {
        let id = "00000000-0000-0000-0000-000000000001";
        let legacy = json!({"id":id,"name":"Home","owner":id,"revision":3});
        let guild: Guild = serde_json::from_value(legacy.clone()).unwrap();
        assert!(guild.icon_id.is_none());
        assert_eq!(serde_json::to_value(&guild).unwrap(), legacy);
        let current = json!({"id":id,"name":"Home","owner":id,"revision":3,"icon_id":id});
        let guild: Guild = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(guild.icon_id.unwrap().to_string(), id);
        assert_eq!(serde_json::to_value(&guild).unwrap(), current);
        let set = json!({"guild_id":id,"revision":3,"image_base64":null});
        let parsed: SetGuildIcon = serde_json::from_value(set.clone()).unwrap();
        assert!(parsed.image_base64.is_none());
        assert_eq!(serde_json::to_value(parsed).unwrap(), set);
        assert!(
            serde_json::from_value::<SetGuildIcon>(
                json!({"guild_id":id,"revision":3,"image_base64":null,"path":"x"})
            )
            .is_err()
        );
    }

    #[test]
    fn finite_and_unlimited_invites_round_trip() {
        let id = "00000000-0000-0000-0000-000000000001";
        for (expiry, max_uses) in [(json!("2026-10-24T00:00:00Z"), 10), (json!(null), 0)] {
            let wire = json!({"id":id,"created_by":id,"created_at":"2026-10-23T00:00:00Z","expires_at":expiry,"max_uses":max_uses,"uses":0,"revoked":false});
            let invite: GuildInvite = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(invite).unwrap(), wire);
        }
        let wire = json!({"action":"create_invite","guild_id":id,"revision":1,"duration_seconds":0,"max_uses":0});
        let command: PermissionRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), wire);
    }

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

    #[test]
    fn server_passwords_are_plain_strings_and_redacted_in_debug() {
        let set = GuildChange::SetPassword {
            password: Some(GuildPassword("hunter2 secret".into())),
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            json!({"action":"set_password","password":"hunter2 secret"})
        );
        assert!(!format!("{set:?}").contains("hunter2"));
        let clear: GuildChange =
            serde_json::from_value(json!({"action":"set_password","password":null})).unwrap();
        assert!(matches!(clear, GuildChange::SetPassword { password: None }));
        // Older servers omit the flag.
        let state: GuildState = serde_json::from_value(json!({
            "guild":{"id":"00000000-0000-0000-0000-000000000001","name":"G",
                "owner":"00000000-0000-0000-0000-000000000002","revision":1},
            "roles":[],"members":[],"channels":[],"overrides":[]
        }))
        .unwrap();
        assert!(!state.password_protected);
    }

    #[test]
    fn nickname_permissions_only_reach_clients_that_declare_their_set() {
        assert_eq!(
            serde_json::to_value(Permission::ChangeNickname).unwrap(),
            "change_nickname"
        );
        assert_eq!(
            serde_json::to_value(Permission::ManageNicknames).unwrap(),
            "manage_nicknames"
        );
        // Every name a 0.1.61 client knows stays in set 1.
        assert_eq!(Permission::ALL.iter().filter(|p| p.set() == 1).count(), 20);
        assert!(Permission::ALL.iter().all(|p| p.set() <= PERMISSION_SET));
        assert_eq!(permissions_path(), "/api/v1/permissions?permission_set=2");
        let id = "00000000-0000-0000-0000-000000000001";
        let mut response = PermissionResponse::Effective {
            permissions: Permission::ALL.into_iter().collect(),
            revision: 1,
        };
        response.restrict(1);
        let PermissionResponse::Effective { permissions, .. } = &response else {
            unreachable!()
        };
        assert!(!permissions.contains(&Permission::ChangeNickname));
        assert_eq!(permissions.len(), 20);
        let stored: Permissions = [Permission::SendMessages, Permission::ManageNicknames].into();
        assert_eq!(
            hidden(&stored, 1).collect::<Vec<_>>(),
            [Permission::ManageNicknames]
        );
        assert_eq!(hidden(&stored, PERMISSION_SET).count(), 0);
        // Older clients' role edits omit appearance; the server keeps it.
        let legacy =
            json!({"action":"edit_role","role_id":id,"name":"Mods","position":3,"permissions":[]});
        let GuildChange::EditRole { appearance, .. } =
            serde_json::from_value(legacy.clone()).unwrap()
        else {
            unreachable!()
        };
        assert!(appearance.is_none());
        let styled = json!({"action":"create_role","name":"Mods","position":3,"permissions":[],
            "appearance":{"color":0xe04f9b,"hoist":true}});
        let parsed: GuildChange = serde_json::from_value(styled.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), styled);
        let nick = json!({"action":"set_nickname","account_id":id,"nickname":null});
        let parsed: GuildChange = serde_json::from_value(nick.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), nick);
        let role: Role = serde_json::from_value(
            json!({"id":id,"name":"Old","position":1,"permissions":[],"everyone":false}),
        )
        .unwrap();
        assert_eq!(role.appearance(), RoleAppearance::default());
        assert!(RoleColor::new(0xff_ffff).is_some() && RoleColor::new(0x100_0000).is_none());
        assert_eq!(RoleColor::new(0x0a0b0c).unwrap().rgb(), (10, 11, 12));
        let home: GuildHome =
            serde_json::from_value(json!({"guild":{"id":id,"name":"G","owner":id,"revision":1},
            "channels":[],"can_manage_roles":false}))
            .unwrap();
        assert!(!home.can_change_nickname);
        assert!(crate::account::valid_display_name(" Ada "));
        for bad in ["", "   ", "a\u{7}b", &"x".repeat(MAX_NICKNAME_CHARS + 1)] {
            assert!(!crate::account::valid_display_name(bad), "{bad:?}");
        }
    }
}
