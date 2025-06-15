use shared::{DownloadProgress, UpdateState};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

use crate::Error;

#[tauri::command]
pub async fn check_updates(app: tauri::AppHandle) {
    // Check for updates and emit the state
    let _ = check_for_updates(&app).await;
}

pub async fn check_for_updates(app: &AppHandle) -> Result<(), Error> {
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
