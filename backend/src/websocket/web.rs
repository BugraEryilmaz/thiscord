use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::extract::ws::{Message::Text, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum_login::login_required;
use shared::{Packet, Split, WebRTCConnection, WebSocketMessage};
use ringbuf::HeapRb;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Sender, channel};

use crate::Error;
use crate::channels::{ROOM_SIZE, VoiceRooms};
use crate::models::{AuthSession, Backend};

pub fn router() -> axum::Router {
    axum::Router::new()
        .route("/", axum::routing::any(post::ws_connection))
        .route_layer(login_required!(Backend))
}

mod post {
    use shared::WebSocketError;

    use shared::models::PermissionType;

    use crate::models::user::{OnlineUser, OnlineUsers};

    use super::*;
    pub async fn ws_connection(
        ws: WebSocketUpgrade,
        auth: AuthSession,
    ) -> impl axum::response::IntoResponse {
        // Check if the user is authenticated
        if auth.user.is_none() {
            tracing::warn!("User is not authenticated");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized".to_string(),
            )
                .into_response();
        };

        ws.on_upgrade(async move |mut socket| {
            // Create a new WebRTC connection
            let mut web_rtc_connection = None;
            let (tx, mut rx) = channel::<WebSocketMessage>(100);
            // Add the user to the online users list
            let online_users = OnlineUsers::get_or_init();
            let user = auth.user.as_ref().unwrap();
            let user = OnlineUser::new(user.0.clone(), tx.clone());
            online_users.add_user(user);
            loop {
                tokio::select! {
                    msg = socket.recv() => {
                        if msg.is_none() {
                            tracing::warn!("WebSocket connection closed");
                            break;
                        }
                        let msg = msg.unwrap();
                        if let Err(err) = msg {
                            tracing::error!("Error receiving WebSocket message: {}", err);
                            continue;
                        }
                        let msg = msg.unwrap();
                        let msg = match msg {
                            Text(msg) => msg,
                            _ => {
                                tracing::error!("Received unsupported WebSocket message type");
                                continue;
                            }
                        };
                        let msg = serde_json::from_str::<WebSocketMessage>(&msg);
                        if let Err(err) = msg {
                            tracing::error!("Failed to parse WebSocket message: {}", err);
                            continue;
                        }
                        let msg = msg.unwrap();
                        match handle_recv(msg, &auth, &mut web_rtc_connection, tx.clone()).await {
                            Ok(_) => {}
                            Err(err) => {
                                tracing::error!("Failed to handle WebSocket message: {}", err);
                            }
                        }
                    },
                    req = rx.recv() => {
                        if req.is_none() {
                            tracing::warn!("WebSocket message channel closed");
                            break;
                        }
                        let req = req.unwrap();
                        handle_send(req, &mut socket).await;
                    }
                }
            }
        })
    }

    async fn handle_send(msg: WebSocketMessage, socket: &mut WebSocket) {
        let serialized = serde_json::to_string(&msg).unwrap();
        if let Err(err) = socket.send(Text(serialized.into())).await {
            tracing::error!("Failed to send WebSocket message: {}", err);
        }
    }

    pub async fn handle_recv(
        msg: WebSocketMessage,
        auth: &AuthSession,
        web_rtc_connection: &mut Option<WebRTCConnection>,
        socket: Sender<WebSocketMessage>,
    ) -> Result<(), Error> {
        // Handle the WebSocket message here
        let backend = &auth.backend;
        let user = (&auth.user).as_ref().unwrap();
        let online_user = OnlineUsers::get_or_init()
            .users
            .get(&user.0.id)
            .unwrap_or_else(|| {
                // Should never happen, but just in case
                let online_user = OnlineUser::new(user.0.clone(), socket.clone());
                let online_users = OnlineUsers::get_or_init();
                online_users.add_user(online_user);
                online_users.users.get(&user.0.id).unwrap()
            });
        let online_user = online_user.value();
        match msg {
            WebSocketMessage::JoinAudioChannel {
                server_id,
                channel_id,
            } => {
                tracing::info!("Joining audio channel: {}", channel_id);
                // Check if the channel exists
                let channel = backend.get_channel(server_id, channel_id)?;
                if channel.is_none() {
                    tracing::error!("Channel does not exist: {}", channel_id);
                    socket
                        .send(WebSocketMessage::Error {
                            err: WebSocketError::NotFound,
                        })
                        .await?;
                    return Err(WebSocketError::NotFound.into());
                }
                let channel = channel.unwrap();
                let permission = if channel.hidden {
                    PermissionType::JoinAudioChannelInHiddenChannels
                } else {
                    PermissionType::JoinAudioChannel
                };
                if !backend
                    .has_permission(user, server_id, permission, None)?
                {
                    tracing::error!("User {} not authorized to join channel: {}", user.0.id, channel_id);
                    socket
                        .send(WebSocketMessage::Error {
                            err: WebSocketError::NotAuthorized,
                        })
                        .await?;
                    return Err(WebSocketError::NotAuthorized.into());
                }
                // Initialize the WebRTC connection (dropping the previous one if it exists)
                *web_rtc_connection = Some(WebRTCConnection::new().await?);
                let web_rtc_connection = web_rtc_connection.as_ref().unwrap();
                // Create audio tracks for the user
                let recv_tracks = web_rtc_connection.create_audio_track_rtp(ROOM_SIZE).await?;
                // Disconnect old audio channel
                if let Some(old_channel_id) = online_user.get_audio_channel() {
                    tracing::info!("Leaving audio channel: {}", old_channel_id);
                    let old_room = VoiceRooms::get_or_init().get_room_or_init(old_channel_id);
                    if let Err(err) = old_room.leave_person(user.0.id).await {
                        tracing::error!("Failed to leave audio channel: {}", err);
                    }
                }
                // Join the voice room
                let room = VoiceRooms::get_or_init().get_room_or_init(channel_id);
                let person_id = room.join_person(&user.0, recv_tracks).await?;
                online_user.set_audio_channel(channel_id);
                // Set up the data forwarding
                let tracks = room.get_track_i_of_all(person_id).await;
                let (prod, cons) = HeapRb::<Packet>::new(100).split();
                let dropped = Arc::new(AtomicBool::new(false));
                web_rtc_connection
                    .background_receive_data(Arc::new(Mutex::new(prod)), dropped.clone());
                web_rtc_connection.background_stream_data(cons, dropped.clone(), tracks);
                // Create the callbacks for the WebRTC connection
                web_rtc_connection
                    .peer_connection
                    .on_ice_connection_state_change(Box::new(move |state| {
                        tracing::debug!("ICE connection state: {:?}", state);
                        Box::pin(async {})
                    }));
                web_rtc_connection
                    .peer_connection
                    .on_peer_connection_state_change(Box::new(move |state| {
                        tracing::debug!("Peer connection state: {:?}", state);
                        Box::pin(async move {})
                    }));
                let socket_clone = socket.clone();
                web_rtc_connection.setup_ice_handling(move |ws_candidate| {
                    let socket_clone = socket_clone.clone();
                    async move {
                        match socket_clone.send(ws_candidate).await {
                            Ok(_) => tracing::debug!("Sent ICE candidate",),
                            Err(err) => {
                                tracing::error!("Failed to send ICE candidate: {}", err);
                            }
                        }
                    }
                });

                // Create and send the WebRTC offer
                let offer = web_rtc_connection.create_offer().await?;
                socket.send(offer).await?;
            }
            WebSocketMessage::WebRTCOffer(_) => {
                tracing::warn!("Received WebRTC offer as the server, this should not happen");
            }
            WebSocketMessage::WebRTCAnswer(rtcsession_description) => {
                tracing::info!("Received WebRTC answer: {:?}", rtcsession_description);
                // Logic to handle the WebRTC answer
                // This could involve setting the remote description, etc.
                if let Some(web_rtc_connection) = web_rtc_connection {
                    web_rtc_connection
                        .peer_connection
                        .set_remote_description(rtcsession_description)
                        .await?;
                } else {
                    tracing::error!("WebRTC connection is not initialized");
                    return Err(Error::WebRTCConnectionNotInitialized);
                }
            }
            WebSocketMessage::IceCandidate(rtcice_candidate_init) => {
                tracing::info!("Received ICE candidate: {:?}", rtcice_candidate_init);
                // Logic to handle the ICE candidate
                // This could involve adding the candidate to the peer connection
                if let Some(web_rtc_connection) = web_rtc_connection {
                    web_rtc_connection
                        .peer_connection
                        .add_ice_candidate(rtcice_candidate_init)
                        .await?;
                } else {
                    tracing::error!("WebRTC connection is not initialized");
                    return Err(Error::WebRTCConnectionNotInitialized);
                }
            }
            WebSocketMessage::DisconnectFromAudioChannel => {
                if let Some(online_user) = OnlineUsers::get_or_init().users.get(&user.0.id) {
                    let online_user = online_user.value();
                    if let Some(channel_id) = online_user.get_audio_channel() {
                        tracing::info!("Disconnecting from audio channel: {}", channel_id);
                        let room = VoiceRooms::get_or_init().get_room_or_init(channel_id);
                        if let Err(err) = room.leave_person(user.0.id).await {
                            tracing::error!("Failed to leave audio channel: {}", err);
                        }
                    } else {
                        tracing::warn!("User {} is not in any audio channel", user.0.id);
                    }
                    online_user.clear_audio_channel();
                }
            },
            WebSocketMessage::Disconnect => todo!(),
            WebSocketMessage::Error { err } => {
                tracing::error!("WebSocket received error: {:?}", err);
            }
        }

        Ok(())
    }
}
