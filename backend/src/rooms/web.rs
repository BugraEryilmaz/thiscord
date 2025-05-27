use axum::Router;
use axum::extract::ws::WebSocket;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::IntoResponse;
use axum::routing::any;

use futures_util::stream::StreamExt;

pub fn router() -> Router {
    Router::new().route("/join_room/{_uuid}", any(self::post::join_room))
}

mod post {

    use std::sync::Arc;

    use axum::extract::Path;
    use my_web_rtc::{Reader, Writer};
    use uuid::Uuid;

    use crate::rooms::Room;
    use crate::rooms::Rooms;

    use super::*;

    pub async fn join_room(ws: WebSocketUpgrade, Path(_uuid): Path<Uuid>) -> impl IntoResponse {
        // Upgrade the request to a WebSocket connection
        ws.on_upgrade(move |ws| handle_room_ws(ws, _uuid))
    }

    pub async fn handle_room_ws(ws: WebSocket, uuid: Uuid) {
        let (sender, receiver) = ws.split();
        // Convert the sender to the expected type
        let web_rtc_connection = Arc::new(
            my_web_rtc::WebRTCConnection::new_with_writer(Writer::Server(sender))
                .await
                .expect("Failed to create WebRTC connection"),
        );
        let room = match Rooms::get_or_init().rooms.get(&uuid) {
            Some(room) => room.value().clone(),
            None => {
                let room = Room::new(uuid);
                Rooms::get_or_init().rooms.insert(uuid, room.clone());
                room
            }
        };
        match room.join_user(web_rtc_connection.clone()).await {
            Ok(_) => tracing::info!("User joined room {}", uuid),
            Err(e) => {
                tracing::error!("Failed to join room {}: {}", uuid, e);
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
