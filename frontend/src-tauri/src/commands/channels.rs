use front_shared::{ChannelWithUsers, URL};
use tauri::Manager;
use uuid::Uuid;

use crate::utils::handle_auth_error;

#[tauri::command(rename_all = "snake_case")]
pub async fn get_channels(
    server_id: Uuid,
    handle: tauri::AppHandle,
) -> Result<Vec<ChannelWithUsers>, String> {
    let state = handle.state::<crate::AppState>();
    let client = state.client.clone();
    let url = format!("https://{}/channels/{}/list", URL, server_id);
    let response = client.get(&url).send().await;

    let resp = handle_auth_error(response, handle)
        .await
        .map_err(|e| e.to_string())?;

    let channels: Vec<ChannelWithUsers> = resp.json().await.map_err(|e| e.to_string())?;

    Ok(channels)
}
