pub const URL: &str = "https://localhost:8081";

use std::{fmt::Display, ops::Not};

use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use uuid::Uuid;

// #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
// pub struct SessionCookie(pub Option<String>);


#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateState {
    Checking,
    Downloading,
    Installing,
    Completed,
    Error(String),
}
impl FromEvent for UpdateState {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress(pub u32);
impl FromEvent for DownloadProgress {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoginStatus {
    LoggedIn,
    LoggedOut,
}
impl FromEvent for LoginStatus {}
impl Not for LoginStatus {
    type Output = bool;

    fn not(self) -> Self::Output {
        matches!(self, LoginStatus::LoggedOut)
    }
}
impl From<bool> for LoginStatus {
    fn from(value: bool) -> Self {
        if value {
            LoginStatus::LoggedIn
        } else {
            LoginStatus::LoggedOut
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server{
    pub id: Uuid,
    pub name: String,
    pub image: String,
}

impl Display for UpdateState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            UpdateState::Checking => write!(f, "Checking for updates..."),
            UpdateState::Downloading => write!(f, "Downloading updates..."),
            UpdateState::Installing => write!(f, "Installing updates..."),
            UpdateState::Completed => write!(f, "Updates completed successfully."),
            UpdateState::Error(err) => write!(f, "Error occurred: {}", err),
        }
    }
}

#[derive(Deserialize)]
struct EventParams<T> {
    payload: T,
}

pub trait FromEvent {
    fn from_event_js(event: JsValue) -> Result<Self, serde_wasm_bindgen::Error> 
    where
        Self: Sized + serde::de::DeserializeOwned
    {
        let ev: EventParams<Self> = serde_wasm_bindgen::from_value(event)?;
        Ok(ev.payload)
    }
    
}
