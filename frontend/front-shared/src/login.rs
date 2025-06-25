use std::ops::Not;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::FromEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: i32,
    pub token: String,
    pub user_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoginStatus {
    LoggedIn(Session),
    LoggedOut,
}
impl FromEvent for LoginStatus {}
impl Not for LoginStatus {
    type Output = bool;

    fn not(self) -> Self::Output {
        matches!(self, LoginStatus::LoggedOut)
    }
}

impl Default for Session {
    fn default() -> Self {
        Session {
            id: 0,
            token: String::new(),
            user_id: Uuid::default(),
        }
    }
}