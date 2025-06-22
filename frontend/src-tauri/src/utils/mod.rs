mod err;
mod update;

use std::sync::Arc;

use diesel::prelude::*;
pub use err::*;
use reqwest::{cookie::Jar, Client};
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc::Sender, Mutex};
pub use update::*;

use crate::{audio::AudioElement, websocket::WebSocketRequest};

pub struct AppState {
    // Define any shared state here
    pub audio_element: AudioElement,
    pub websocket: Mutex<Sender<WebSocketRequest>>,
    pub client: Client,
    pub cookie_store: Arc<Jar>,
}

impl AppState {
    pub fn new(websocket: Mutex<Sender<WebSocketRequest>>) -> Self {
        let cookie_store = Arc::new(Jar::default());
        let client = Client::builder()
            .cookie_provider(cookie_store.clone())
            .build()
            .expect("Failed to create HTTP client");
        Self {
            audio_element: AudioElement::new(),
            websocket,
            client,
            cookie_store,
        }
    }
}

pub fn establish_connection(handle: &AppHandle) -> SqliteConnection {
    let path = handle
        .path()
        .data_dir()
        .unwrap()
        .join("thiscord/")
        .join("db.sqlite");
    SqliteConnection::establish(path.to_str().unwrap()).expect("Error connecting to database")
}
