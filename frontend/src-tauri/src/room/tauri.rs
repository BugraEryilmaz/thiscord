use std::sync::Arc;

use crate::{audio::AudioElement, AppState};
use my_web_rtc::WebRTCConnection;
use ringbuf::{traits::Split, HeapRb};
use tauri::Manager;
pub use crate::utils::Error;
use uuid::Uuid;

#[tauri::command]
pub async fn join_room(app_handle: tauri::AppHandle, room_id: Uuid) {
    let app_state = app_handle.state::<AppState>();
    if let Some(audio_element) = app_state.audio_element.write().unwrap().take() {
        match audio_element.quit() {
            Ok(_) => tracing::info!("Audio element already exists, stopped it before joining a new room."),
            Err(e) => tracing::error!("Failed to stop audio element: {}", e),
        }
    }
    if let Some(_web_rtc_connection) = app_state.web_rtc_connection.read().await.clone() {
        _web_rtc_connection.close().await;
        tracing::info!("WebRTC connection already exists, closing it before joining a new room.");
    }
    // Create the audio element and start the input/output streams
    let (mic_producer, mic_consumer) = HeapRb::<i16>::new(12000).split();
    let mut audio_element = AudioElement::new(mic_producer).unwrap();
    audio_element.start_input_stream().unwrap();
    audio_element.start_output_stream().unwrap();

    // Create the WebRTC connection and set up the audio tracks
    let web_rtc_connection = WebRTCConnection::new().await.unwrap();
    let audio_track = web_rtc_connection
        .create_audio_track_sample(10)
        .await
        .unwrap()
        .iter()
        .enumerate()
        .map(|(idx, track)| {
            if idx == 0 {
                Arc::new(tokio::sync::Mutex::new(Some(track.clone())))
            } else {
                Arc::new(tokio::sync::Mutex::new(None))
            }
        })
        .collect::<Vec<_>>();

    web_rtc_connection
        .background_stream_audio(mic_consumer, audio_track)
        .await
        .unwrap();
    web_rtc_connection
        .background_receive_audio(audio_element.speaker_consumers.clone())
        .await
        .unwrap();

    web_rtc_connection
        .peer_connection
        .on_ice_connection_state_change(Box::new(move |state| {
            tracing::info!("ICE connection state: {:?}", state);
            Box::pin(async {})
        }));

    web_rtc_connection
        .peer_connection
        .on_peer_connection_state_change(Box::new(move |state| {
            tracing::info!("Peer connection state: {:?}", state);
            Box::pin(async move {
            })
        }));
    let web_rtc_connection = Arc::new(web_rtc_connection);
    web_rtc_connection
        .clone()
        .connect_ws(format!("wss://192.168.1.126:8081/rooms/join_room/{}", room_id).as_str())
        .await
        .unwrap();
    web_rtc_connection.offer().await.unwrap();
    web_rtc_connection.setup_ice_handling().await.unwrap();
    app_state
        .audio_element
        .write()
        .unwrap()
        .replace(audio_element);
    app_state
        .web_rtc_connection
        .write()
        .await
        .replace(web_rtc_connection);
    tracing::info!("Joined room {}", room_id);
}


#[tauri::command]
pub async fn leave_room(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    if let Some(web_rtc_connection) = app_state.web_rtc_connection.write().await.take() {
        web_rtc_connection.close().await;
        tracing::info!("Left room and closed WebRTC connection.");
    } else {
        tracing::warn!("No WebRTC connection to close.");
    }
    
    if let Some(audio_element) = app_state.audio_element.write().unwrap().take() {
        audio_element.quit().unwrap();
        tracing::info!("Stopped audio streams.");
    } else {
        tracing::warn!("No audio element to stop.");
    }
    tracing::info!("Left room and stopped audio streams.");
}