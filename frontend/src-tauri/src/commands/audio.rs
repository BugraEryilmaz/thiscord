use front_shared::AudioDevices;
use tauri::Manager;

use crate::{audio::{AudioCommand, AudioElement}, websocket::WebSocketRequest, AppState};

#[tauri::command]
pub async fn mute_microphone(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::Mute))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send mute command: {}", e));
    }
}

#[tauri::command]
pub async fn unmute_microphone(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::Unmute))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send unmute command: {}", e));
    }
}

#[tauri::command]
pub async fn deafen_speaker(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::Deafen))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send deafen command: {}", e));
    }
}

#[tauri::command]
pub async fn undeafen_speaker(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::Undeafen))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send undeafen command: {}", e));
    }
}

#[tauri::command]
pub async fn get_devices(state: tauri::State<'_, AppState>) -> Result<AudioDevices, String> {
    let speakers = AudioElement::list_speakers()
        .map_err(|e| e.to_string())?;
    let mics = AudioElement::list_mics()
        .map_err(|e| e.to_string())?;
    Ok(AudioDevices {
        mics: mics,
        mic: state.current_mic.lock().unwrap().clone(),
        speakers: speakers,
        speaker: state.current_speaker.lock().unwrap().clone(),
    })
}

#[tauri::command]
pub async fn set_mic(app_handle: tauri::AppHandle, device: String) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::SetMic(device)))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send set mic command: {}", e));
    }
}

#[tauri::command]
pub async fn set_speaker(app_handle: tauri::AppHandle, device: String) {
    let app_state = app_handle.state::<AppState>();
    let ws = &app_state.websocket;
    {
        let ws = ws.read().await;
        ws.send(WebSocketRequest::AudioCommand(AudioCommand::SetSpeaker(device)))
            .await
            .map_err(|e| e.to_string())
            .unwrap_or_else(|e| eprintln!("Failed to send set speaker command: {}", e));
    }
}