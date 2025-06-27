#[cfg(feature = "diesel")]
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::servers))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub connection_string: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::servers))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct ServerWithoutID {
    pub name: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::servers))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct ServerFull {
    pub id: Uuid,
    pub name: String,
    pub connection_string: String,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
}