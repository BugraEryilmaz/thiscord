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
    use shared::{Reader, Writer};
    use uuid::Uuid;

    use crate::models::AuthSession;
    use crate::rooms::Room;
    use crate::rooms::Rooms;

    use super::*;

    pub async fn join_room(ws: WebSocketUpgrade, Path(_uuid): Path<Uuid>, auth: AuthSession) -> impl IntoResponse {
        // check if the user is authenticated
        if auth.user.is_none() {
            tracing::warn!("User is not authenticated");
            return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized".to_string()).into_response();
        }
        let _backend = auth.backend;
        // backend.
        // Upgrade the request to a WebSocket connection
        ws.on_upgrade(move |ws| handle_room_ws(ws, _uuid))
    }

    pub async fn handle_room_ws(ws: WebSocket, uuid: Uuid) {
        let (sender, receiver) = ws.split();
        // Convert the sender to the expected type
        let web_rtc_connection = Arc::new(
            shared::WebRTCConnection::new_with_writer(Writer::Server(sender))
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
        let room_id = match room.join_user(web_rtc_connection.clone()).await {
            Ok(rid) => {tracing::info!("User joined room {}", uuid); rid},
            Err(e) => {
                tracing::error!("Failed to join room {}: {}", uuid, e);
                return;
            }
        };
        
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
            Box::pin(async move {
                let room = match Rooms::get_or_init().rooms.get(&uuid) {
                    Some(room) => room.value().clone(),
                    None => {
                        let room = Room::new(uuid);
                        Rooms::get_or_init().rooms.insert(uuid, room.clone());
                        room
                    }
                };

                if state == shared::RTCPeerConnectionState::Closed {
                    if let Err(e) = room.leave_user(room_id).await {
                        tracing::error!("Failed to leave room {}: {}", uuid, e);
                    } else {
                        tracing::info!("User left room {}", uuid);
                    }
                }
            })
        }));
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
