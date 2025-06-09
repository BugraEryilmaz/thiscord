pub mod audio;
pub mod room;
pub mod utils;

use std::sync::RwLock as StdRwLock;
use std::{sync::Arc, vec};

use audio::tauri::*;
use audio::AudioElement;
use my_web_rtc::WebRTCConnection;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::Client;
use room::tauri::*;
use shared::LoginRequest;
use shared::{DownloadProgress, UpdateState, URL};
use tauri::http::HeaderValue;
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::UpdaterExt;
use tokio::spawn;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
pub use utils::Error;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

pub struct AppState {
    // Define any shared state here
    audio_element: StdRwLock<Option<AudioElement>>,
    web_rtc_connection: RwLock<Option<Arc<WebRTCConnection>>>,
    client: Client,
    cookie_store: Arc<Jar>,
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

#[tauri::command]
async fn test_emit(app: tauri::AppHandle) {
    // Emit an event to the frontend
    app.emit("update_state", UpdateState::Downloading).unwrap();
}

#[tauri::command]
async fn check_updates(app: tauri::AppHandle) {
    // Check for updates and emit the state
    let _ = check_for_updates(&app).await;
}

#[tauri::command]
async fn login(username: String, password: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let client = &state.client;
    let _response = client
        .post(format!("{}/auth/login", URL))
        .json(&LoginRequest { username, password })
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let cookie = state.cookie_store.clone();
    let cookie = cookie.cookies(&Url::parse(URL).unwrap()).unwrap_or(HeaderValue::from_static(""));
    tracing::info!("Cookies after login: {:?}", cookie);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub async fn run() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,my_web_rtc=info", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    // Initialize the WebRTC connection
    tauri::Builder::default()
        .manage(AppState::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let handle = app.handle().clone();
            spawn(async move {
                let handle = handle;
                while check_for_updates(&handle).await.is_err() {
                    tracing::error!("Failed to check for updates, retrying in 5 seconds...");
                    sleep(std::time::Duration::from_secs(5)).await;
                }
            });
            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            let cookie = state.cookie_store.clone();
            let cookie = cookie.cookies(&Url::parse(URL).unwrap()).unwrap_or(HeaderValue::from_static(""));
            tracing::info!("Cookies: {:?}", cookie);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            join_room,
            mute_microphone,
            unmute_microphone,
            deafen_speaker,
            undeafen_speaker,
            test_emit,
            check_updates,
            login,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn check_for_updates(app: &AppHandle) -> Result<(), Error> {
    let updater = app.updater().expect("Updater plugin not initialized");
    match updater.check().await {
        Ok(Some(update)) => {
            app.emit("update_state", UpdateState::Downloading).unwrap();
            let mut total_received: u64 = 0;
            update
                .download_and_install(
                    |received, all| {
                        if let Some(all) = all {
                            total_received += received as u64;
                            let percentage = total_received * 100 / all;
                            app.emit("download_progress", DownloadProgress(percentage as u32))
                                .unwrap();
                        }
                    },
                    || {
                        app.emit("update_state", UpdateState::Installing).unwrap();
                    },
                )
                .await?;
            app.emit("update_state", UpdateState::Completed).unwrap();
        }
        Ok(None) => {
            app.emit("update_state", UpdateState::Completed).unwrap();
        }
        Err(e) => {
            app.emit("update_state", UpdateState::Error(e.to_string()))
                .unwrap();
            return Err(Error::from(e));
        }
    }
    Ok(())
}
