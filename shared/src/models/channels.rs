#[cfg(feature = "diesel")]
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(diesel_derive_enum::DbEnum))]
#[cfg_attr(feature = "diesel", ExistingTypePath = "crate::schema::sql_types::ChannelType")]
#[cfg_attr(feature = "diesel", DbValueStyle = "PascalCase")]
pub enum ChannelType {
    Text,
    Voice,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::channels))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct Channel {
    pub id: Uuid,
    pub name: String,
    pub type_: ChannelType,
    pub hidden: bool,
    pub server_id: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct VoiceUser {
    pub id: Uuid,
    pub username: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelWithUsers {
    pub channel: Channel,
    pub users: Vec<VoiceUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioChannelMemberUpdate {
    pub channel: Channel,
    pub user: VoiceUser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable, Insertable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::channels))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct NewChannel {
    pub name: String,
    pub type_: ChannelType,
    pub hidden: bool,
    pub server_id: Uuid,
}   

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinChannel {
    pub server_id: Uuid,
    pub channel_id: Uuid,
    pub channel_name: String,
}