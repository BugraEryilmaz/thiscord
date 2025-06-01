use crate::models::{PermissionType, PermissionsOfUser, Users};
use crate::schema;
use crate::{
    Error,
    models::{Backend, Credentials},
};
use argon2::{PasswordHash, PasswordVerifier};
use async_trait::async_trait;
use axum_login::{AuthUser, AuthnBackend, UserId};
use diesel::prelude::*;

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
impl AuthnBackend for Backend {
    type User = Users;
    type Error = Error;
    type Credentials = Credentials;

    async fn authenticate(
        &self,
        Credentials { username, password }: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let mut conn = self.get_connection()?;
        let user = schema::users::table
            .filter(schema::users::username.eq(username))
            .first::<Users>(&mut conn)
            .optional()?;
        if user.is_none() {
            return Ok(None);
        }
        let user = user.unwrap();
        if user.deleted || user.activated == false {
            tracing::info!("User {} is deleted or not activated", user.username);
            return Ok(None);
        }
        let parsed_hash = PasswordHash::new(user.password.as_str())?;
        let argon2 = argon2::Argon2::default();
        if argon2
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
        {
            Ok(Some(user))
        } else {
            Ok(None)
        }
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        let mut conn = self.get_connection()?;
        let user = schema::users::table
            .filter(schema::users::id.eq(user_id))
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

impl Backend {
    pub async fn has_permission(
        &self,
        user: <Backend as AuthnBackend>::User,
        server_id: uuid::Uuid,
        permission: PermissionType,
    ) -> Result<bool, Error> {
        let mut conn = self.get_connection()?;

        // If the query returns a result, the user has the permission
        schema::roles::table
            .inner_join(schema::permissions::table)
            .inner_join(schema::user_roles::table)
            .filter(schema::user_roles::user_id.eq(user.id))
            .filter(schema::user_roles::server_id.eq(server_id))
            .filter(schema::roles::server_id.eq(server_id))
            .filter(schema::permissions::type_.eq(permission))
            .select(schema::permissions::id)
            .first::<uuid::Uuid>(&mut conn)
            .optional()
            .map(|res| res.is_some())
            .map_err(|e| e.into())
    }

    pub async fn get_user_permissions(
        &self,
        user: <Backend as AuthnBackend>::User,
        server_id: uuid::Uuid,
    ) -> Result<PermissionsOfUser, Error> {
        let mut conn = self.get_connection()?;

        // Get all permissions for the user on the specified server
        schema::roles::table
            .left_join(schema::permissions::table)
            .inner_join(schema::user_roles::table)
            .filter(schema::user_roles::user_id.eq(user.id))
            .filter(schema::user_roles::server_id.eq(server_id))
            .select((schema::roles::name, schema::permissions::type_.nullable()))
            .load::<(String, Option<PermissionType>)>(&mut conn)
            .map_err(|e| Error::from(e))
            .map(|permissions| {
                let role = permissions
                    .first()
                    .map_or("none".to_string(), |(role, _)| role.clone());
                let permission_types = permissions
                    .into_iter()
                    .filter_map(|(_, perm)| perm)
                    .collect();
                PermissionsOfUser {
                    role,
                    permission_type: permission_types,
                }
            })
    }
}
