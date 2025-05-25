use axum::Router;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::IntoResponse;
use axum::routing::any;

use futures_util::stream::StreamExt;

pub fn router() -> Router {
    Router::new().route("/join_room", any(self::post::join_room))
}

mod post {

    use std::sync::Arc;

    use my_web_rtc::{Reader, Writer};
    use ringbuf::{traits::Split, HeapRb};
    use tokio::sync::Mutex;

    use super::*;

    pub async fn join_room(ws: WebSocketUpgrade) -> impl IntoResponse {
        // Upgrade the request to a WebSocket connection
        ws.on_upgrade(handle_room_ws)
    }

    pub async fn handle_room_ws(ws: WebSocket) {
        let (sender, receiver) = ws.split();
        // Convert the sender to the expected type
        let web_rtc_connection = Arc::new(
            my_web_rtc::WebRTCConnection::new_with_writer(Arc::new(Mutex::new(Writer::Server(
                sender,
            ))))
            .await
            .expect("Failed to create WebRTC connection"),
        );

        let (tx, rx) = HeapRb::<i16>::new(48000).split();
        match web_rtc_connection.background_receive_audio(tx).await {
            Ok(_) => tracing::info!("Background audio receive started"),
            Err(e) => {
                tracing::error!("Failed to start background audio receive: {}", e);
                return;
            }
        }
        match web_rtc_connection.background_stream_audio(rx).await {
            Ok(_) => tracing::info!("Background audio stream started"),
            Err(e) => {
                tracing::error!("Failed to start background audio stream: {}", e);
                return;
            }
        }
        match web_rtc_connection
            .create_handler(Reader::Server(receiver))
            .await
        {
            Ok(_) => tracing::info!("WebRTC connection handler created"),
            Err(e) => {
                tracing::error!("Failed to create WebRTC connection handler: {}", e);
                return;
            }
        }
        tracing::info!("WebRTC connection established, waiting for messages...");
        // Handle incoming messages from the WebSocket
    }
}
