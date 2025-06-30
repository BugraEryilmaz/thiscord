pub const URL: &str = "localhost:8081";

mod update;
use shared::models::AudioChannelMemberUpdate;
pub use update::{DownloadProgress, UpdateState};

mod login;
pub use login::{LoginStatus, Session};

mod status;
pub use status::*;

use serde::{Deserialize};
use wasm_bindgen::JsValue;

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

impl FromEvent for AudioChannelMemberUpdate {}