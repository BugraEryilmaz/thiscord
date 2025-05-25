pub mod audio;
pub mod utils;

use std::{sync::Arc, vec};

use audio::AudioElement;
use my_web_rtc::WebRTCConnection;
use ringbuf::{traits::Split, HeapRb};
pub use utils::Error;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub async fn run() {
    // Initialize the WebRTC connection
    let mut audio_element = AudioElement::new().unwrap();
    let (tx, rx) = HeapRb::<i16>::new(48000).split();
    let (tx_srv, rx_srv) = HeapRb::<i16>::new(48000).split();
    audio_element.start_input_stream(tx).unwrap();
    // audio_element.start_output_stream(rx).unwrap();
    audio_element.start_output_stream(rx_srv).unwrap();
    let web_rtc_connection = Arc::new(WebRTCConnection::new().await.unwrap());
    
    web_rtc_connection.background_stream_audio(rx).await.unwrap();
    web_rtc_connection.background_receive_audio(tx_srv).await.unwrap();

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
    web_rtc_connection
        .clone()
        .connect_ws("wss://localhost:8081/webrtc/join_room")
        .await
        .unwrap();
    web_rtc_connection.offer().await.unwrap();
    web_rtc_connection.setup_ice_handling().await.unwrap();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
