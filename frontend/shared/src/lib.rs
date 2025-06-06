use std::fmt::Display;

use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

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
