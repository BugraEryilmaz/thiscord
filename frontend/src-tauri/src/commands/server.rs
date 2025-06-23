use reqwest::multipart;
use front_shared::{URL};
use shared::models::Server;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::utils::{handle_auth_error, AppState};

#[tauri::command(rename_all = "snake_case")]
pub async fn create_server(
    app: tauri::AppHandle,
    name: String,
    image_url: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let client = &state.client;
    let form = if let Some(image_url) = image_url {
        multipart::Form::new()
            .text("server-name", name)
            .file("server-image", image_url)
            .await
            .map_err(|e| e.to_string())?
    } else {
        multipart::Form::new().text("server-name", name)
    };
    let resp = client
        .post(format!("https://{}/servers/create-server", URL))
        .multipart(form)
        .send()
        .await;
    let _ = handle_auth_error(resp, app).await.map_err(|e| e.to_string())?;
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
    let resp = state
        .client
        .get(format!("https://{}/servers/get-servers", URL))
        .send()
        .await;

    let resp = handle_auth_error(resp, app)
        .await
        .map_err(|e| e.to_string())?;

    let servers: Vec<Server> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(servers)
}

#[tauri::command]
pub async fn pick_file(app: tauri::AppHandle) -> Option<FilePath> {
    app.dialog()
        .file()
        .add_filter("Images", &["jpg", "png", "jpeg"])
        .blocking_pick_file()
}
