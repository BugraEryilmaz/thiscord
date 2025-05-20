use argon2::{PasswordHash, PasswordVerifier};
use axum_login::{AuthUser, AuthnBackend, UserId};
use crate::{err::Error, models::{Credentials, PostgresBackend}};
use crate::models::Users;
use diesel::prelude::*;
use crate::schema::users;
use async_trait::async_trait;

impl AuthUser for Users {
    type Id = uuid::Uuid;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.password.as_bytes()
    }
}

#[async_trait]
impl AuthnBackend for PostgresBackend {
    type User = Users;
    type Error = Error;
    type Credentials = Credentials;
    
    async fn authenticate(
        &self,
        Credentials { username, password }: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let mut conn = self.get_connection()?;
        let user = users::table
            .filter(users::username.eq(username))
            .first::<Users>(&mut conn)
            .optional()?;
        if user.is_none() {
            return Ok(None);
        }
        let user = user.unwrap();
        if user.deleted {
            return Ok(None);
        }
        let parsed_hash = PasswordHash::new(user.password.as_str())?;
        let argon2 = argon2::Argon2::default();
        if argon2.verify_password(password.as_bytes(), &parsed_hash).is_ok() {
            Ok(Some(user))
        } else {
            Ok(None)
        }
    }
    
    async fn get_user(
        &self,
        user_id: &UserId<Self>,
    ) -> Result<Option<Self::User>, Self::Error> {
        let mut conn = self.get_connection()?;
        let user = users::table
            .filter(users::id.eq(user_id))
            .first::<Users>(&mut conn)
            .optional()?;
        if let Some(ref user) = user {
            if user.deleted {
                return Ok(None);
            }
        }
        Ok(user)
    }
}
