use front_shared::{URL};
use shared::models::ChannelWithUsers;
use tauri::Manager;
use uuid::Uuid;

use crate::{utils::handle_auth_error, websocket::WebSocketRequest};

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


#[tauri::command(rename_all = "snake_case")]
pub async fn join_channel(
    channel_id: Uuid,
    server_id: Uuid,
    channel_name: String,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = handle.state::<crate::AppState>();
    let ws = &state.websocket;

    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::JoinAudioChannel { server_id, channel_id, channel_name })
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn disconnect_call(handle: tauri::AppHandle) -> Result<(), String> {
    let state = handle.state::<crate::AppState>();
    let ws = &state.websocket;

    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::DisconnectFromAudioChannel).await.map_err(|e| e.to_string())?;
    }

    Ok(())
}