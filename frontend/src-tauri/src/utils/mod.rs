mod err;
mod update;

use std::sync::Arc;

use diesel::prelude::*;
pub use err::*;
use front_shared::Status;
use reqwest::{cookie::Jar, Client};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc::Sender, RwLock};
use std::sync::Mutex as StdMutex;
pub use update::*;

use crate::{audio::AudioElement, websocket::WebSocketRequest};

pub struct AppState {
    // Define any shared state here
    pub audio_element: AudioElement,
    pub websocket: RwLock<Sender<WebSocketRequest>>,
    pub client: Client,
    pub cookie_store: Arc<Jar>,
    pub conn_status: Arc<StdMutex<Status>>,
}

impl AppState {
    pub fn new(websocket: RwLock<Sender<WebSocketRequest>>) -> Self {
        let cookie_store = Arc::new(Jar::default());
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .cookie_provider(cookie_store.clone())
            .build()
            .expect("Failed to create HTTP client");
        Self {
            audio_element: AudioElement::new(),
            websocket,
            client,
            cookie_store,
            conn_status: Arc::new(StdMutex::new(Status::Offline)),
        }
    }

    pub fn change_status(&self, status: Status, handle: &AppHandle) {
        {
            let mut conn_status = self.conn_status.lock().unwrap();
            *conn_status = status.clone();
        }
        let _ = handle.emit("status_change", status);
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
