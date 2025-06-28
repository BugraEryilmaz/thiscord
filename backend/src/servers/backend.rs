use std::collections::HashSet;

use crate::{models::{user::OnlineUser, Backend}, servers::UsersActiveServers, utils::SubscribableOnce, Error};
use shared::{models::{ChannelType, NewChannel, PermissionType, Server, DEFAULT_OWNER_PERMISSIONS, DEFAULT_USER_PERMISSIONS}, schema};
use diesel::prelude::*;
use rand::{Rng, distr::Alphanumeric};
use uuid::Uuid;

impl Backend {
    pub fn create_server(
        &self,
        server_name: &str,
        server_image: Option<String>,
        user_id: Uuid,
    ) -> Result<String, Error> {
        let connection_string = self.create_connection_string()?;
        let mut conn = self.get_connection()?;
        let image_path = server_image.clone();
        let image_url = server_image
            .map(|image| {
                // get the filename from the image path
                let image = image
                    .rsplit('/')
                    .next()
                    .unwrap_or(image.as_str());
                // save the image folder as a static path
                format!("static/server/{}", image)
            });

        let server_id = diesel::insert_into(schema::servers::table)
            .values((
                schema::servers::name.eq(server_name),
                schema::servers::connection_string.eq(&connection_string),
                schema::servers::image_url.eq(image_url),
                schema::servers::image_path.eq(image_path),
            ))
            .returning(schema::servers::id)
            .get_result::<Uuid>(&mut conn)
            .map_err(|e| Error::from(e))?;

        self.join_user_to_server(user_id, server_id)?;
        let owner_role = self.create_role("owner".to_string(), server_id, DEFAULT_OWNER_PERMISSIONS.iter())?;
        self.add_user_role(user_id, server_id, owner_role)?;
        let _user_role =
            self.create_role("user".to_string(), server_id, DEFAULT_USER_PERMISSIONS.iter())?;

        // Create the default channels for the server
        let default_channels = vec![
            NewChannel {
                name: "General".to_string(),
                server_id,
                type_: ChannelType::Text,
                hidden: false,
            },
            NewChannel {
                name: "Voice".to_string(),
                server_id,
                type_: ChannelType::Voice,
                hidden: false,
            },
        ];
        for channel in default_channels {
            self.create_channel(&channel)?;
        }

        Ok(connection_string)
    }

    pub fn join_user_to_server(&self, user_id: Uuid, server_id: Uuid) -> Result<(), Error> {
        let mut conn = self.get_connection()?;
        diesel::insert_into(schema::joined_users::table)
            .values((
                schema::joined_users::user_id.eq(user_id),
                schema::joined_users::server_id.eq(server_id),
            ))
            .execute(&mut conn)
            .map_err(|e| Error::from(e))?;
        // Ensure the user has the default user role
        let user_role = self.get_role_id(server_id, "user")?;
        if let Some(role_id) = user_role {
            self.add_user_role(user_id, server_id, role_id)?;
        } 
        Ok(())
    }

    pub fn create_role<'a>(
        &self,
        role: String,
        server_id: Uuid,
        permissions: impl Iterator<Item = &'a PermissionType>,
    ) -> Result<Uuid, Error> {
        let mut conn = self.get_connection()?;
        let owner_role = diesel::insert_into(schema::roles::table)
            .values((
                schema::roles::name.eq(role),
                schema::roles::server_id.eq(server_id),
            ))
            .returning(schema::roles::id)
            .get_result::<Uuid>(&mut conn)
            .map_err(|e| Error::from(e))?;
        let owner_permissions = permissions
            .map(|perm| {
                (
                    schema::permissions::role_id.eq(owner_role),
                    schema::permissions::type_.eq(perm),
                )
            })
            .collect::<Vec<_>>();

        diesel::insert_into(schema::permissions::table)
            .values(owner_permissions)
            .execute(&mut conn)
            .map_err(|e| Error::from(e))?;

        Ok(owner_role)
    }

    pub fn get_role_id(
        &self,
        server_id: Uuid,
        role_name: &str,
    ) -> Result<Option<Uuid>, Error> {
        let mut conn = self.get_connection()?;
        schema::roles::table
            .filter(schema::roles::server_id.eq(server_id))
            .filter(schema::roles::name.eq(role_name))
            .select(schema::roles::id)
            .first::<Uuid>(&mut conn)
            .optional()
            .map_err(|e| Error::from(e))
    }

    
    pub fn add_user_role(
        &self,
        user_id: Uuid,
        server_id: Uuid,
        role_id: Uuid,
    ) -> Result<(), Error> {
        let mut conn = self.get_connection()?;
        diesel::insert_into(schema::user_roles::table)
            .values((
                schema::user_roles::user_id.eq(user_id),
                schema::user_roles::server_id.eq(server_id),
                schema::user_roles::role_id.eq(role_id),
            ))
            .execute(&mut conn)
            .map_err(|e| Error::from(e))?;
        Ok(())
    }

    pub fn create_connection_string(&self) -> Result<String, Error> {
        let mut conn = self.get_connection()?;
        for _ in 0..10 {
            // create a random connection string of length 8
            let connection_string = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(8)
                .map(char::from)
                .collect::<String>();
            // Check if the connection string already exists
            let exists = schema::servers::table
                .filter(schema::servers::connection_string.eq(&connection_string))
                .select(schema::servers::connection_string)
                .first::<String>(&mut conn)
                .optional()?;
            if exists.is_none() {
                return Ok(connection_string);
            }
        }
        Err(Error::ConnectionStringGenerationFailed)
    }

    pub fn get_server_by_connection_string(
        &self,
        connection_string: &str,
    ) -> Result<Option<Uuid>, Error> {
        let mut conn = self.get_connection()?;
        schema::servers::table
            .filter(schema::servers::connection_string.eq(connection_string))
            .select(schema::servers::id)
            .first::<Uuid>(&mut conn)
            .optional()
            .map_err(|e| Error::from(e))
    }

    pub fn get_servers_for_user(&self, user_id: Uuid) -> Result<Vec<Server>, Error> {
        let mut conn = self.get_connection()?;
        schema::servers::table
            .inner_join(schema::joined_users::table)
            .filter(schema::joined_users::user_id.eq(user_id))
            .select(Server::as_select())
            .load::<Server>(&mut conn)
            .map_err(|e| Error::from(e))
    }

    pub fn get_server(&self, server_id: Uuid) -> Result<Server, Error> {
        let mut conn = self.get_connection()?;
        schema::servers::table
            .filter(schema::servers::id.eq(server_id))
            .select(Server::as_select())
            .first::<Server>(&mut conn)
            .map_err(|e| Error::from(e))
    }

    pub fn get_server_from_channel(&self, channel_id: Uuid) -> Result<Server, Error> {
        let mut conn = self.get_connection()?;
        schema::channels::table
            .inner_join(schema::servers::table.on(schema::servers::id.eq(schema::channels::server_id)))
            .filter(schema::channels::id.eq(channel_id))
            .select(Server::as_select())
            .first::<Server>(&mut conn)
            .map_err(|e| Error::from(e))
    }
}

impl SubscribableOnce for Server {
    fn subscribe(&self, user: &OnlineUser) {
        UsersActiveServers::get().add_user_to_server(user, self);
    }

    fn unsubscribe(user: &OnlineUser) {
        UsersActiveServers::get().remove_user_from_server(user);
    }

    fn get_subscribers(&self) -> HashSet<OnlineUser> {
        UsersActiveServers::get().get_users_for_server(self)
    }

    fn get_subscribed(user: &OnlineUser) -> Option<Self> {
        UsersActiveServers::get().get_server_for_user(user)
            .map(|server| server.clone())
    }
}