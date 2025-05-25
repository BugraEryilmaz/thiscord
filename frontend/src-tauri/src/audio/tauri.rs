use tauri::Manager;

use crate::AppState;

#[tauri::command]
pub async fn mute_microphone(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    match app_state.audio_element.mute() {
        Ok(_) => println!("Microphone muted successfully."),
        Err(e) => eprintln!("Failed to mute microphone: {}", e),
    }
}

#[tauri::command]
pub async fn unmute_microphone(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    match app_state.audio_element.unmute() {
        Ok(_) => println!("Microphone unmuted successfully."),
        Err(e) => eprintln!("Failed to unmute microphone: {}", e),
    }
}

#[tauri::command]
pub async fn deafen_speaker(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    match app_state.audio_element.deafen() {
        Ok(_) => println!("Speaker deafened successfully."),
        Err(e) => eprintln!("Failed to deafen speaker: {}", e),
    }
}

#[tauri::command]
pub async fn undeafen_speaker(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    match app_state.audio_element.undeafen() {
        Ok(_) => println!("Speaker undeafened successfully."),
        Err(e) => eprintln!("Failed to undeafen speaker: {}", e),
    }
}
