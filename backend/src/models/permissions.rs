use strum_macros::EnumIter;

#[derive(diesel_derive_enum::DbEnum, Debug, Clone, PartialEq, Eq, EnumIter)]
#[ExistingTypePath = "crate::schema::sql_types::PermissionType"]
#[DbValueStyle = "PascalCase"]
pub enum PermissionType {
    DeleteServer,
    CreateChannel,
    DeleteChannel,
    CreateHiddenChannel,
    DeleteHiddenChannel,
    ListHiddenChannels,
    AdjustChannelPermissions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsOfUser {
    pub role: String,
    pub permission_type: Vec<PermissionType>,
}