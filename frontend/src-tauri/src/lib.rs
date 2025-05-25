pub mod audio;
pub mod utils;

use std::{sync::Arc, vec};

use audio::tauri::*;
use audio::AudioElement;
use my_web_rtc::WebRTCConnection;
use ringbuf::{traits::Split, HeapRb};
use tauri::Manager;
pub use utils::Error;
use uuid::Uuid;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
async fn join_room(app_handle: tauri::AppHandle, room_id: Uuid) {
    let app_state = app_handle.state::<AppState>();
    let web_rtc_connection = app_state.web_rtc_connection.clone();

    web_rtc_connection
        .clone()
        .connect_ws(format!("wss://localhost:8081/rooms/join_room/{}", room_id).as_str())
        .await
        .unwrap();
    web_rtc_connection.offer().await.unwrap();
    web_rtc_connection.setup_ice_handling().await.unwrap();
}

struct AppState {
    // Define any shared state here
    audio_element: AudioElement,
    web_rtc_connection: Arc<WebRTCConnection>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub async fn run() {
    // Initialize the WebRTC connection
    let (tx, rx) = HeapRb::<i16>::new(12000).split();
    let (tx_srv, rx_srv) = HeapRb::<i16>::new(12000).split();
    let mut audio_element = AudioElement::new(tx, rx_srv).unwrap();
    audio_element.start_input_stream().unwrap();
    audio_element.start_output_stream().unwrap();
    let web_rtc_connection = WebRTCConnection::new().await.unwrap();

    web_rtc_connection
        .background_stream_audio(rx)
        .await
        .unwrap();
    web_rtc_connection
        .background_receive_audio(tx_srv)
        .await
        .unwrap();

    web_rtc_connection
        .peer_connection
        .on_ice_connection_state_change(Box::new(move |state| {
            println!("ICE connection state: {:?}", state);
            Box::pin(async {})
        }));

    web_rtc_connection
        .peer_connection
        .on_peer_connection_state_change(Box::new(move |state| {
            println!("Peer connection state: {:?}", state);
            Box::pin(async {})
        }));

    tauri::Builder::default()
        .manage(AppState {
            audio_element,
            web_rtc_connection: Arc::new(web_rtc_connection),
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            join_room,
            mute_microphone,
            unmute_microphone,
            deafen_speaker,
            undeafen_speaker
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
