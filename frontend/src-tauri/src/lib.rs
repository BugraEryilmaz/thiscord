pub mod audio;
pub mod room;
pub mod utils;

use std::sync::RwLock as StdRwLock;
use std::{sync::Arc, vec};

use audio::tauri::*;
use audio::AudioElement;
use my_web_rtc::WebRTCConnection;
use room::tauri::*;
use shared::{DownloadProgress, UpdateState};
use tauri::{AppHandle, Emitter};
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
}

#[tauri::command]
async fn test_emit(app: tauri::AppHandle) {
    // Emit an event to the frontend
    app.emit("update_state", UpdateState::Downloading).unwrap();
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
        .manage(AppState {
            audio_element: StdRwLock::new(None),
            web_rtc_connection: RwLock::new(None),
        })
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            join_room,
            mute_microphone,
            unmute_microphone,
            deafen_speaker,
            undeafen_speaker,
            test_emit,
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
