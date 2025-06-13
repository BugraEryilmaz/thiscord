use shared::{Server, URL};
use tauri::Manager;

use crate::{commands::logout, utils::AppState};

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

    if let Err(e) = resp {
        tracing::error!("Failed to fetch servers: {}", e);
        if let Some(status) = e.status() {
            if status.as_str() == "401" {
                logout(app.clone()).await?;
                return Err("Unauthorized access. Please log in again.".to_string());
            }
            return Err(format!("Failed to fetch servers: {}", e));
        }
        return Err(format!("Failed to fetch servers: {}", e));
    }
    let resp = resp.unwrap();

    let servers: Vec<Server> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(servers)
}