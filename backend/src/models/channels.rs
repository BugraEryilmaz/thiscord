use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;
use uuid::Uuid;

#[derive(diesel_derive_enum::DbEnum, Debug, Clone, PartialEq, Eq, EnumIter, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ChannelType"]
#[DbValueStyle = "PascalCase"]
pub enum ChannelType {
    Text,
    Voice,
}


#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::channels)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Channel {
    pub id: Uuid,
    pub name: String,
    pub type_: ChannelType,
    pub hidden: bool,
    pub server_id: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::channels)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NewChannel {
    pub name: String,
    pub type_: ChannelType,
    pub hidden: bool,
    pub server_id: Uuid,
}   
