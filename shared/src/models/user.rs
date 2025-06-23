
#[cfg(feature = "diesel")]
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;


#[derive(Clone)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable, Insertable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::users))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct Users {
    pub id: uuid::Uuid,
    pub username: String,
    pub email: String,
    pub password: String,
    pub deleted: bool,
    pub created_at: chrono::NaiveDateTime,
    pub activated: bool,
}

#[derive(Clone)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable, Insertable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::user_activations))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct Activation {
    pub activation_code: String,
    pub user_id: Uuid,
}

#[derive(Clone)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable, Insertable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::user_activations))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct ActivationFull {
    pub id: Uuid,
    pub user_id: Uuid,
    pub activation_code: String,
    pub valid_until: Option<chrono::NaiveDateTime>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", derive(Queryable, Selectable, Insertable))]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::users))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::pg::Pg)))]
pub struct Signup {
    pub username: String,
    pub email: String,
    pub password: String,
}

impl std::fmt::Debug for Users {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Users")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("email", &self.email)
            .field("deleted", &self.deleted)
            .field("created_at", &self.created_at)
            .finish()
    }
}