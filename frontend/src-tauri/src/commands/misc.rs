use front_shared::Status;
use tauri::Manager;

use crate::utils::AppState;

#[tauri::command]
pub async fn get_status(handle: tauri::AppHandle) -> Result<Status, String> {
    let state = handle.state::<AppState>();
    let conn_status = state.conn_status.lock().unwrap();
    Ok(conn_status.clone())
}
