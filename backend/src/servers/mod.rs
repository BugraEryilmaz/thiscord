use std::{collections::HashSet, sync::OnceLock};

use dashmap::DashMap;
use shared::models::Server;
use uuid::Uuid;

use crate::models::user::OnlineUser;

pub mod web;
pub mod backend;

pub struct UsersActiveServers {
    pub user_to_server_map: DashMap<Uuid, Server>,
    pub server_to_user_map: DashMap<Uuid, HashSet<OnlineUser>>,
}

static USERS_ACTIVE_SERVERS: OnceLock<UsersActiveServers> = OnceLock::new();

impl UsersActiveServers {
    pub fn get() -> &'static UsersActiveServers {
        USERS_ACTIVE_SERVERS.get_or_init(|| UsersActiveServers {
            user_to_server_map: DashMap::new(),
            server_to_user_map: DashMap::new(),
        })
    }

    pub fn add_user_to_server(&self, user: &OnlineUser, server: &Server) {
        let server_id = server.id;
        self.user_to_server_map.insert(user.user.id, server.clone());
        {
            // Ensure the server entry exists in the server_to_user_map
            self.server_to_user_map.entry(server_id).or_insert_with(HashSet::new);
        }
        self.server_to_user_map.alter(&server_id, |_, mut users| {
            users.insert(user.clone());
            users
        });
    }

    pub fn remove_user_from_server(&self, user: &OnlineUser) {
        let server_id = self.user_to_server_map.remove(&user.user.id);
        if let Some((_, server)) = server_id {
            self.server_to_user_map.remove(&server.id);
        }
    }

    pub fn get_server_for_user(&self, user: &OnlineUser) -> Option<Server> {
        self.user_to_server_map.get(&user.user.id).map(|entry| entry.value().clone())
    }

    pub fn get_users_for_server(&self, server: &Server) -> HashSet<OnlineUser> {
        self.server_to_user_map.get(&server.id).map(|entry| entry.value().clone()).unwrap_or_default()
    }
}