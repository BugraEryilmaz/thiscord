mod err;
mod update;

use std::sync::{Arc, RwLock as StdRwLock};

use diesel::prelude::*;
pub use err::Error;
use my_web_rtc::WebRTCConnection;
use reqwest::{cookie::Jar, Client};
use tokio::sync::RwLock;
pub use update::*;
use tauri::{AppHandle, Manager};

use crate::audio::AudioElement;


pub struct AppState {
    // Define any shared state here
    pub audio_element: StdRwLock<Option<AudioElement>>,
    pub web_rtc_connection: RwLock<Option<Arc<WebRTCConnection>>>,
    pub client: Client,
    pub cookie_store: Arc<Jar>,
}

impl AppState {
    pub fn new() -> Self {
        let cookie_store = Arc::new(Jar::default());
        let client = Client::builder()
            .cookie_provider(cookie_store.clone())
            .build()
            .expect("Failed to create HTTP client");
        Self {
            audio_element: StdRwLock::new(None),
            web_rtc_connection: RwLock::new(None),
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
