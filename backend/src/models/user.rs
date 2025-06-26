use std::{cell::Ref, sync::{Arc, Mutex as StdMutex, OnceLock}};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use dashmap::DashMap;
use diesel::prelude::*;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::Error;
use shared::{
    WebSocketMessage,
    models::{Activation, ActivationFull, Signup, Users},
    schema::{user_activations, users},
};

use super::Backend;

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
        let new_user = diesel::insert_into(shared::schema::users::table)
            .values(&new_user)
            .returning(users::all_columns)
            .get_result::<Users>(&mut conn)
            .map_err(|e| {
                tracing::error!("Failed to create user: {}", e);
                Error::Database(e)
            })?;

        // Create 6 digit code
        let user = new_user.clone();
        let backend = Arc::new(self.clone());
        tokio::spawn(async move {
            backend
                .create_activation(user.id, &user.email)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("Failed to create activation code: {}", e);
                });
        });
        Ok(new_user)
    }

    pub async fn create_activation(&self, user_id: Uuid, user_email: &str) -> Result<(), Error> {
        let activation_code = rand::random::<u32>() % 1_000_000;
        let activation_code = format!("{:06}", activation_code);
        let mut conn = self.get_connection()?;
        // delete if user has previous activation code
        diesel::delete(user_activations::table.filter(user_activations::user_id.eq(user_id)))
            .execute(&mut conn)?;
        // Send email to user with the code
        self.email
            .send_email(
                user_email,
                "Thiscord activation code",
                &format!(
                    "Your activation code is: {}\n\nLink: https://{}/auth/activate?token={}",
                    activation_code,
                    std::env::var("HOST").unwrap_or("localhost".to_string()),
                    activation_code
                ),
            )
            .await?;
        // Save code to database
        let activation = Activation {
            activation_code: activation_code.to_string(),
            user_id: user_id,
        };
        diesel::insert_into(shared::schema::user_activations::table)
            .values(&activation)
            .execute(&mut conn)
            .map_err(|e| {
                tracing::error!("Failed to create activation code: {}", e);
                Error::Database(e)
            })?;
        Ok(())
    }

    pub fn try_activate_user(self, token: &str) -> Result<Option<()>, Error> {
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

    pub fn get_user_by_username(&self, username: &str) -> Result<Option<Users>, Error> {
        let mut conn = self.get_connection()?;
        let user = users::table
            .filter(users::username.eq(username))
            .first::<Users>(&mut conn)
            .optional()?;
        Ok(user)
    }
}

static ONLINE_USERS: OnceLock<OnlineUsers> = OnceLock::new();

pub struct OnlineUsers {
    pub users: DashMap<Uuid, OnlineUser>,
}
impl OnlineUsers {
    pub fn get_or_init() -> &'static OnlineUsers {
        ONLINE_USERS.get_or_init(|| OnlineUsers {
            users: DashMap::new(),
        })
    }

    pub fn add_user(&self, user: OnlineUser) {
        self.users.insert(user.user.id, user);
    }
}

pub struct OnlineUser {
    pub user: Users,
    pub websocket: Sender<WebSocketMessage>,
    pub audio_channel: StdMutex<Option<Uuid>>,
}

impl OnlineUser {
    pub fn new(user: Users, websocket: Sender<WebSocketMessage>) -> Self {
        Self {
            user,
            websocket,
            audio_channel: StdMutex::new(None),
        }
    }
    pub fn set_audio_channel(&self, channel_id: Uuid) {
        let mut audio_channel = self.audio_channel.lock().unwrap();
        *audio_channel = Some(channel_id);
    }
    pub fn clear_audio_channel(&self) {
        let mut audio_channel = self.audio_channel.lock().unwrap();
        *audio_channel = None;
    }
    pub fn get_audio_channel(&self) -> Option<Uuid> {
        let audio_channel = self.audio_channel.lock().unwrap();
        *audio_channel
    }
}
