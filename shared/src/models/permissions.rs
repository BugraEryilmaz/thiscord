use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "diesel", derive(diesel_derive_enum::DbEnum))]
#[cfg_attr(feature = "diesel", ExistingTypePath = "crate::schema::sql_types::PermissionType")]
#[cfg_attr(feature = "diesel", DbValueStyle = "PascalCase")]
pub enum PermissionType {
    DeleteServer,
    CreateChannel,
    DeleteChannel,
    CreateHiddenChannel,
    DeleteHiddenChannel,
    ListHiddenChannels,
    AdjustChannelPermissions,
    ListChannels,
    ListUsersInServer,
    JoinAudioChannel,
    JoinAudioChannelInHiddenChannels,
    SendMessages,
    SendMessagesInHiddenChannels,
    DeleteMessages,
    DeleteMessagesSelf,
}

pub struct PermissionContext {
    pub user_id: Uuid,
    pub resource_owner_id: Uuid,
}

impl PermissionType {
    pub fn requires_owner(&self) -> bool {
        matches!(self, PermissionType::DeleteMessagesSelf)
    }
    pub fn permission_check(&self, context: Option<&PermissionContext>) -> bool {
        if !self.requires_owner() {
            return true;
        }
        if let Some(context) = context {
            if self.requires_owner() && context.user_id == context.resource_owner_id {
                return true;
            }
        }
        false
    }
}

use std::{collections::HashSet, sync::LazyLock};

pub static DEFAULT_OWNER_PERMISSIONS: LazyLock<Vec<PermissionType>> = LazyLock::new(|| {
    // Collect all permissions into a vector
    PermissionType::iter().collect::<Vec<_>>()
});

pub static DEFAULT_USER_PERMISSIONS: &[PermissionType] = &[
    PermissionType::ListChannels,
    PermissionType::ListUsersInServer,
    PermissionType::JoinAudioChannel,
    PermissionType::SendMessages,
    PermissionType::DeleteMessagesSelf,
];

#[derive(Debug, Clone, Serialize)]
pub struct PermissionsOfUser {
    pub user_id: Uuid,
    pub role: String,
    pub permission_type: HashSet<PermissionType>,
}