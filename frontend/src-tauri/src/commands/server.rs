use shared::{Server, URL};
use tauri::Manager;

use crate::utils::{handle_request_error, AppState};

#[tauri::command]
pub async fn create_server(_app: tauri::AppHandle) -> Result<(), String> {

    Ok(())
}

#[tauri::command]
pub async fn join_server(_app: tauri::AppHandle, _connection_string: String) -> Result<(), String> {
    // Implementation for joining a server
    Ok(())
}

#[tauri::command]
pub async fn get_servers(app: tauri::AppHandle) -> Result<Vec<Server>, String> {
    // Implementation for fetching the list of joined servers
    let state = app.state::<AppState>();
    let resp = state.client
        .get(format!("{}/servers/get-servers", URL))
        .send()
        .await;

    let resp = handle_request_error(resp, app).await?;

    let servers: Vec<Server> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(servers)
}