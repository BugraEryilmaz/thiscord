pub const URL: &str = "localhost:8081";

mod update;
pub use update::{DownloadProgress, UpdateState};

mod login;
pub use login::{LoginStatus, Session};

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
