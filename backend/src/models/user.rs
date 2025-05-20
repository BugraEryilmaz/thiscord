use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use argon2::{
    password_hash::{
        rand_core::OsRng,
        PasswordHasher, SaltString
    },
    Argon2
};

use crate::{err::Error, schema::users};

use super::PostgresBackend;

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Users {
    pub id: uuid::Uuid,
    pub username: String,
    pub email: String,
    pub password: String,
    pub deleted: bool,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Insertable)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Signup {
    pub username: String,
    pub email: String,
    pub password: String,
}

impl PostgresBackend {

    pub fn check_username_exists(
        &self,
        username: &str,
    ) -> Result<bool, Error> {
        let mut conn = self.get_connection()?;
        let exists = users::table
            .filter(users::username.eq(username))
            .first::<Users>(&mut conn)
            .optional()?
            .is_some();
        Ok(exists)
    }

    pub fn check_email_exists(
        &self,
        email: &str,
    ) -> Result<bool, Error> {
        let mut conn = self.get_connection()?;
        let exists = users::table
            .filter(users::email.eq(email))
            .first::<Users>(&mut conn)
            .optional()?
            .is_some();
        Ok(exists)
    }
    
    pub fn create_user(
        &self,
        Signup { username, email, password }: Signup,
    ) -> Result<Users, Error> {
        let mut conn = self.get_connection()?;
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password = argon2
            .hash_password(password.as_bytes(), &salt)?;
        let password = password.to_string();
        let new_user = Signup {
            username,
            email,
            password,
        };
        diesel::insert_into(crate::schema::users::table)
            .values(&new_user)
            .returning(users::all_columns)
            .get_result::<Users>(&mut conn)
            .map_err(|e| {
                tracing::error!("Failed to create user: {}", e);
                e.into()
            })
        }
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