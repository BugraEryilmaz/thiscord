use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Error,
    schema::{user_activations, users},
};

use super::Backend;

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
    pub activated: bool,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::user_activations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Activation {
    pub activation_code: String,
    pub user_id: Uuid,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::user_activations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
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

#[derive(Clone, Debug, Deserialize, Serialize, Insertable)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Signup {
    pub username: String,
    pub email: String,
    pub password: String,
}

impl Backend {
    pub fn check_username_exists(&self, username: &str) -> Result<bool, Error> {
        let mut conn = self.get_connection()?;
        let exists = users::table
            .filter(users::username.eq(username))
            .first::<Users>(&mut conn)
            .optional()?
            .is_some();
        Ok(exists)
    }

    pub fn check_email_exists(&self, email: &str) -> Result<bool, Error> {
        let mut conn = self.get_connection()?;
        let exists = users::table
            .filter(users::email.eq(email))
            .first::<Users>(&mut conn)
            .optional()?
            .is_some();
        Ok(exists)
    }

    pub async fn create_user(
        &self,
        Signup {
            username,
            email,
            password,
        }: Signup,
    ) -> Result<Users, Error> {
        let mut conn = self.get_connection()?;
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password = argon2.hash_password(password.as_bytes(), &salt)?;
        let password = password.to_string();
        let new_user = Signup {
            username,
            email,
            password,
        };
        let new_user = diesel::insert_into(crate::schema::users::table)
            .values(&new_user)
            .returning(users::all_columns)
            .get_result::<Users>(&mut conn)
            .map_err(|e| {
                tracing::error!("Failed to create user: {}", e);
                Error::Database(e)
            })?;

        // Create 6 digit code
        let code = rand::random::<u32>() % 1_000_000;
        // Send email to user with the code
        self.email
            .send_email(
                &new_user.email,
                "Thiscord activation code",
                &format!(
                    "Your activation code is: {}\n\nLink: https://{}/auth/activate?token={}",
                    code,
                    std::env::var("HOST").unwrap_or("localhost".to_string()),
                    code
                ),
            )
            .await?;
        // Save code to database
        let activation = Activation {
            activation_code: code.to_string(),
            user_id: new_user.id,
        };
        diesel::insert_into(crate::schema::user_activations::table)
            .values(&activation)
            .execute(&mut conn)
            .map_err(|e| {
                tracing::error!("Failed to create activation code: {}", e);
                Error::Database(e)
            })?;
        Ok(new_user)
    }

    pub fn try_activate_user(&self, token: &str) -> Result<Option<()>, Error> {
        let mut conn = self.get_connection()?;
        let user_activation = users::table
            .inner_join(user_activations::table)
            .filter(user_activations::activation_code.eq(token))
            .select((users::all_columns, user_activations::all_columns))
            .first::<(Users, ActivationFull)>(&mut conn)
            .optional()?;
        let (user, activation) = match user_activation {
            Some((user, activation)) => (user, activation),
            None => return Ok(None),
        };
        if activation.valid_until.is_none() {
            return Err(Error::InvalidActivationCode);
        }
        if activation.valid_until.unwrap() < chrono::Utc::now().naive_utc() {
            return Ok(None);
        }
        diesel::delete(user_activations::table.filter(user_activations::user_id.eq(user.id)))
            .execute(&mut conn)?;
        diesel::update(users::table.filter(users::id.eq(user.id)))
            .set(users::activated.eq(true))
            .execute(&mut conn)?;
        Ok(Some(()))
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
