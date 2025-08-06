use std::sync::atomic::Ordering;

use front_shared::{models::user_boost::PerUserBoost, URL};
use shared::models::ChannelWithUsers;
use tauri::Manager;
use uuid::Uuid;

use crate::{utils::{establish_connection, handle_auth_error}, websocket::WebSocketRequest};

#[tauri::command(rename_all = "snake_case")]
pub async fn get_channels(
    server_id: Uuid,
    handle: tauri::AppHandle,
) -> Result<Vec<ChannelWithUsers>, String> {
    let state = handle.state::<crate::AppState>();
    let client = state.client.clone();
    let url = format!("https://{}/channels/{}/list", URL, server_id);
    let response = client.get(&url).send().await;

    let resp = handle_auth_error(response, handle.clone())
        .await
        .map_err(|e| e.to_string())?;

    let mut channels: Vec<ChannelWithUsers> = resp.json().await.map_err(|e| e.to_string())?;

    // Get the boost level for each user in the channels
    let mut conn = establish_connection(&handle);
    channels.iter_mut().for_each(|channel| {
        channel.users.iter_mut().for_each(|user| {
            user.boost = Some(PerUserBoost::get(&mut conn, user.id).boost_level.load(Ordering::Relaxed));
        });
    });

    Ok(channels)
}


#[tauri::command(rename_all = "snake_case")]
pub async fn join_channel(
    channel_with_users: ChannelWithUsers,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = handle.state::<crate::AppState>();
    let ws = &state.websocket;

    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::JoinAudioChannel { channel_with_users })
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