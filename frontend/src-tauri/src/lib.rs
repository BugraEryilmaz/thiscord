pub mod audio;
pub mod utils;
pub mod room;

use std::sync::RwLock as StdRwLock;
use std::{sync::Arc, vec};

use audio::tauri::*;
use room::tauri::*;
use audio::AudioElement;
use my_web_rtc::WebRTCConnection;
use tokio::sync::RwLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
pub use utils::Error;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

pub struct AppState {
    // Define any shared state here
    audio_element: StdRwLock<Option<AudioElement>>,
    web_rtc_connection: RwLock<Option<Arc<WebRTCConnection>>>,
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
        .invoke_handler(tauri::generate_handler![
            join_room,
            mute_microphone,
            unmute_microphone,
            deafen_speaker,
            undeafen_speaker
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
