pub mod audio;
pub mod commands;
pub mod models;
pub mod room;
pub mod schema;
pub mod utils;

use audio::tauri::*;
use commands::*;
use reqwest::cookie::CookieStore;
use room::tauri::*;
use shared::{UpdateState, URL};
use tauri::{Emitter, Manager, Url};
use tokio::spawn;
use tokio::time::sleep;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
pub use utils::Error;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

#[tauri::command]
async fn test_emit(app: tauri::AppHandle) {
    // Emit an event to the frontend
    app.emit("update_state", UpdateState::Downloading).unwrap();
}

use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

use crate::models::Session;
use crate::utils::{check_for_updates, check_updates, establish_connection, AppState};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

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
        .plugin(tauri_plugin_dialog::init())
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

            let path = app
                .path()
                .data_dir()
                .unwrap()
                .join("thiscord/")
                .join("db.sqlite");
            if !path.exists() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::File::create(&path).unwrap();
            }
            let mut conn = establish_connection(app.handle());
            let migrated = conn.run_pending_migrations(MIGRATIONS);

            tracing::info!("Migrated: {:?}", migrated);

            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            let cookie_store = state.cookie_store.clone();
            let cookie = Session::get(conn)
                .map_err(|e| tracing::error!("Failed to get session cookie: {}", e))
                .unwrap_or_default();
            if !cookie.token.is_empty() {
                cookie_store.add_cookie_str(&cookie.token, &Url::parse(URL).unwrap());
            }
            if let Some(cookie) = cookie_store.cookies(&Url::parse(URL).unwrap()) {
                tracing::info!("Cookies found: {:?}", cookie);
            } else {
                tracing::warn!("No cookies found in the cookie store.");
            }
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
            check_cookies,
            signup,
            logout,
            create_server,
            join_server,
            get_servers,
            pick_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
