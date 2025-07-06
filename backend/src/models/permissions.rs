use shared::models::PermissionType;
use strum::IntoEnumIterator;

use std::sync::LazyLock;

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
