use std::sync::{Arc, Mutex as StdMutex};

use front_shared::{CallStatus, Status, URL};
use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector;
use reqwest::cookie::CookieStore;
use reqwest::header;
use ringbuf::HeapProd;
use shared::models::{ChannelWithUsers, TurnCreds};
use shared::{HeapCons, RTCPeerConnectionState, WebRTCConnection, WebSocketMessage, ROOM_SIZE};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::Sender;
use tokio::{select, sync::mpsc::Receiver};
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::client::IntoClientRequest,
    tungstenite::Message::Text, Connector,
};

pub enum WebSocketRequest {
    JoinAudioChannel {
        channel_with_users: ChannelWithUsers,
    },
    DisconnectFromAudioChannel,
    Disconnect,
    AudioCommand(AudioCommand),
}

use crate::audio::{AudioCommand, AudioElement};
use crate::{utils::AppState, Error};

pub async fn websocket_handler(
    handle: AppHandle,
    cmd_rx: &mut Receiver<WebSocketRequest>,
) -> Result<(), Error> {
    let state = handle.state::<AppState>();
    state.change_status(Status::Connecting, &handle);
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel(100);
    let url = format!("wss://{}/websocket", URL);
    let mut web_rtc_connection: Option<WebRTCConnection> = None;
    let mut audio: Option<AudioElement> = Some(AudioElement::new(handle.clone()));

    let mut request = url.into_client_request()?;
    let cookie_store = state.cookie_store.clone();

    if let Some(cookies) =
        cookie_store.cookies(&reqwest::Url::parse(format!("https://{}", URL).as_str()).unwrap())
    {
        request.headers_mut().insert(header::COOKIE, cookies);
    }

    let connector = TlsConnector::builder()
        // .danger_accept_invalid_certs(true)
        // .danger_accept_invalid_hostnames(true)
        .build()?;
    let connector = Connector::NativeTls(connector);
    let (mut ws_stream, _) =
        connect_async_tls_with_config(request, None, false, Some(connector)).await?;
    state.change_status(Status::Online, &handle);
    loop {
        select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(message)) => {
                        let message = match message {
                            Text(text) => {
                                match serde_json::from_str::<WebSocketMessage>(&text) {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        tracing::error!("Failed to parse WebSocket message: {}", e);
                                        continue;
                                    }
                                }
                            },
                            _ => {
                                tracing::warn!("Received non-text message over WebSocket");
                                continue;
                            }
                        };
                        if let Err(e) = handle_websocket_message(message, &mut web_rtc_connection, &mut audio, ws_tx.clone(), handle.clone()).await {
                            tracing::error!("Failed to handle WebSocket message: {}", e);
                            continue;
                        }
                    },
                    Some(Err(e)) => {
                        tracing::error!("WebSocket error: {}", e);
                        break;
                    },
                    None => {
                        tracing::info!("WebSocket connection closed");
                        break;
                    }
                }
            },
            msg = ws_rx.recv() => {
                let msg = match msg {
                    Some(message) => {
                        let msg = serde_json::to_string(&message).unwrap();
                        Text(msg.into())
                    },
                    None => {
                        tracing::error!("WebSocket internal message channel closed");
                        break;
                    }
                };
                if let Err(e) = ws_stream.send(msg).await {
                    tracing::error!("Failed to send message over WebSocket: {}", e);
                    break;
                }
            },
            msg = cmd_rx.recv() => {
                let msg = match msg {
                    Some(message) => {
                        handle_internal_request(message, &mut web_rtc_connection, &mut audio, ws_tx.clone(), handle.clone()).await
                    },
                    None => {
                        tracing::error!("WebSocket internal message channel closed");
                        break;
                    }
                };
                if let Err(e) = msg {
                    tracing::error!("Failed to handle internal request: {}", e);
                    continue;
                }
            }
        }
    }
    state.change_status(Status::Offline, &handle);
    Ok(())
}

pub async fn handle_internal_request(
    request: WebSocketRequest,
    web_rtc_connection: &mut Option<WebRTCConnection>,
    audio: &mut Option<AudioElement>,
    socket: Sender<WebSocketMessage>,
    handle: AppHandle,
) -> Result<(), Error> {
    let state = handle.state::<AppState>();
    match request {
        WebSocketRequest::JoinAudioChannel { channel_with_users } => {
            let channel_id = channel_with_users.channel.id;
            let server_id = channel_with_users.channel.server_id;
            let channel_name = channel_with_users.channel.name.clone();
            // First get TURN credentials
            let client = handle.state::<AppState>().client.clone();
            let resp = client
                .get(format!("https://{}/utils/turn/get-creds", URL))
                .send()
                .await;
            let turn_creds: Option<TurnCreds> = match resp {
                Ok(response) => response.json().await.ok(),
                Err(e) => {
                    tracing::error!("Failed to get TURN credentials: {}", e);
                    None
                }
            };
            *web_rtc_connection = Some(WebRTCConnection::new(channel_id, turn_creds).await?);
            let web_rtc_connection = web_rtc_connection.as_ref().unwrap();
            *audio = Some(AudioElement::new(handle.clone()));
            let audio_element = audio.as_mut().unwrap();
            audio_element.set_channel(&channel_with_users, handle.clone());

            // Start the audio element streams
            let mic_consumer: Arc<StdMutex<HeapCons<f32>>> = audio_element.start_mic()?;
            let speaker_producers: Vec<Arc<StdMutex<HeapProd<f32>>>> =
                audio_element.start_speaker()?;

            // Create the WebRTC streams
            let audio_track = web_rtc_connection
                .create_audio_track_sample(ROOM_SIZE)
                .await?;
            let audio_track = audio_track[0].clone();
            web_rtc_connection
                .background_stream_audio(mic_consumer, audio_track)
                .await?;
            web_rtc_connection
                .background_receive_audio(speaker_producers)
                .await?;

            // Create WebRTC handlers
            web_rtc_connection
                .peer_connection
                .on_ice_connection_state_change(Box::new(move |state| {
                    tracing::debug!("ICE connection state: {:?}", state);
                    Box::pin(async {})
                }));
            let handle_clone = handle.clone();
            let channel_name_clone = channel_name.clone();
            web_rtc_connection
                .peer_connection
                .on_peer_connection_state_change(Box::new(move |state| {
                    tracing::debug!("Peer connection state: {:?}", state);
                    let appstate = handle_clone.state::<AppState>();
                    match state {
                        RTCPeerConnectionState::Connecting => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Connecting),
                                &handle_clone,
                            );
                        }
                        RTCPeerConnectionState::Connected => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Connected),
                                &handle_clone,
                            );
                        }
                        RTCPeerConnectionState::Disconnected => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Disconnected),
                                &handle_clone,
                            );
                        }
                        RTCPeerConnectionState::Failed => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Failed),
                                &handle_clone,
                            );
                        }
                        RTCPeerConnectionState::Closed => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Closed),
                                &handle_clone,
                            );
                        }
                        RTCPeerConnectionState::New | RTCPeerConnectionState::Unspecified => {
                            appstate.change_status(
                                Status::OnCall(channel_name_clone.clone(), CallStatus::Connecting),
                                &handle_clone,
                            );
                        }
                    }
                    Box::pin(async move {})
                }));
            state.change_status(
                Status::OnCall(channel_name.clone(), CallStatus::Connecting),
                &handle,
            );
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

            // Join the audio channel
            let join_message = WebSocketMessage::JoinAudioChannel {
                server_id,
                channel_id,
            };
            if let Err(e) = socket.send(join_message).await {
                tracing::error!("Failed to send join audio channel message: {}", e);
            }
        }
        WebSocketRequest::DisconnectFromAudioChannel => {
            if let Some(web_rtc_connection) = web_rtc_connection.take() {
                web_rtc_connection.close().await;
            }
            if let Some(mut audio_element) = audio.take() {
                audio_element.clear_channel();
                audio_element.quit()?;
            }
            let disconnect_message = WebSocketMessage::DisconnectFromAudioChannel;
            if let Err(e) = socket.send(disconnect_message).await {
                tracing::error!("Failed to send disconnect audio channel message: {}", e);
            }
            state.change_status(Status::Online, &handle);
        }
        WebSocketRequest::Disconnect => todo!(),
        WebSocketRequest::AudioCommand(command) => {
            if let Some(audio_element) = audio {
                match &command {
                    AudioCommand::Mute => {
                        if let Err(e) = audio_element.mute() {
                            tracing::error!("Failed to mute audio: {}", e);
                        }
                    }
                    AudioCommand::Unmute => {
                        if let Err(e) = audio_element.unmute() {
                            tracing::error!("Failed to unmute audio: {}", e);
                        }
                    }
                    AudioCommand::Deafen => {
                        if let Err(e) = audio_element.deafen() {
                            tracing::error!("Failed to deafen audio: {}", e);
                        }
                    }
                    AudioCommand::Undeafen => {
                        if let Err(e) = audio_element.undeafen() {
                            tracing::error!("Failed to undeafen audio: {}", e);
                        }
                    }
                    AudioCommand::Quit => {
                        if let Err(e) = audio_element.quit() {
                            tracing::error!("Failed to quit audio: {}", e);
                        }
                        *audio = None; // Clear the audio element
                    }
                    AudioCommand::SetMic(device_name) => {
                        if let Err(e) = audio_element.change_mic(&device_name, &state) {
                            tracing::error!("Failed to set microphone: {}", e);
                        }
                    }
                    AudioCommand::SetSpeaker(device_name) => {
                        if let Err(e) = audio_element.change_speaker(&device_name, &state) {
                            tracing::error!("Failed to set speaker: {}", e);
                        }
                    }
                    AudioCommand::SetMicBoost(boost) => {
                        audio_element.change_mic_boost(*boost, &state);
                    }
                    AudioCommand::SetSpeakerBoost(boost) => {
                        audio_element.change_speaker_boost(*boost, &state);
                    }
                    AudioCommand::SetUserBoost {
                        user_id,
                        boost_level,
                    } => {
                        if let Err(e) =
                            audio_element.set_user_boost(*user_id, *boost_level, handle.clone())
                        {
                            tracing::error!("Failed to set user boost: {}", e);
                        }
                    }
                }
            }
            match &command {
                AudioCommand::SetMic(device_name) => {
                    AudioElement::set_default_mic(device_name, handle);
                }
                AudioCommand::SetSpeaker(device_name) => {
                    AudioElement::set_default_speaker(device_name, handle);
                }
                AudioCommand::SetMicBoost(boost) => {
                    AudioElement::set_default_mic_boost(*boost, handle);
                }
                AudioCommand::SetSpeakerBoost(boost) => {
                    AudioElement::set_default_speaker_boost(*boost, handle);
                }
                AudioCommand::SetUserBoost {
                    user_id,
                    boost_level,
                } => {
                    if let Err(e) = AudioElement::set_default_user_boost(*user_id, *boost_level, handle) {
                        tracing::error!("Failed to set user boost: {}", e);
                    }
                }
                AudioCommand::Mute
                | AudioCommand::Deafen
                | AudioCommand::Quit
                | AudioCommand::Undeafen
                | AudioCommand::Unmute => {}
            }
        }
    }
    Ok(())
}

pub async fn handle_websocket_message(
    message: WebSocketMessage,
    web_rtc_connection: &mut Option<WebRTCConnection>,
    audio: &mut Option<AudioElement>,
    tx: Sender<WebSocketMessage>,
    handle: AppHandle,
) -> Result<(), Error> {
    match message {
        WebSocketMessage::JoinAudioChannel {
            server_id: _,
            channel_id: _,
        } => {
            tracing::warn!("Received JoinAudioChannel message, but this is client");
        }
        WebSocketMessage::DisconnectFromAudioChannel => {
            tracing::warn!("Received DisconnectFromAudioChannel message, but this is client");
        }
        WebSocketMessage::IceCandidate(candidate) => {
            if let Some(web_rtc_connection) = web_rtc_connection {
                if let Err(e) = web_rtc_connection.add_remote_ice_candidate(candidate).await {
                    tracing::error!("Failed to add ICE candidate: {}", e);
                    return Err(e.into());
                }
            } else {
                tracing::warn!("Received ICE candidate but no WebRTC connection exists");
            }
        }
        WebSocketMessage::WebRTCOffer(rtcsession_description) => {
            if let Some(web_rtc_connection) = web_rtc_connection {
                let answer = web_rtc_connection
                    .create_answer(rtcsession_description)
                    .await;
                if let Err(e) = answer {
                    tracing::error!("Failed to create WebRTC answer: {}", e);
                    return Err(e.into());
                };
                let answer = answer.unwrap();
                if let Err(e) = tx.send(answer).await {
                    tracing::error!("Failed to send WebRTC answer: {}", e);
                    return Err(e.into());
                }
            } else {
                tracing::warn!("Received WebRTC offer but no WebRTC connection exists");
            }
        }
        WebSocketMessage::WebRTCAnswer(_) => {
            tracing::warn!("Received WebRTCAnswer message, but this is client");
        }
        WebSocketMessage::Disconnect => {
            tracing::info!("Received Disconnect message, but this is client");
        }
        WebSocketMessage::Error { err } => {
            tracing::error!("WebSocket error: {}", err);
            return Err(Error::WebSocketError(err));
        }
        WebSocketMessage::SomeoneJoinedAudioChannel { data } => {
            tracing::info!(
                "User {} joined audio channel {} on server {}",
                data.user.username,
                data.channel.id,
                data.channel.server_id
            );
            let handle = handle;
            if let Some(audio_element) = audio {
                if let Err(e) = audio_element.handle_join_channel(&data, handle.clone()) {
                    tracing::error!("Failed to handle join channel: {}", e);
                }
            }
            // Fails only when the event name is invalid
            if handle.emit("someone-joined-audio-channel", data).is_err() {
                tracing::error!("Event name 'someone-joined-audio-channel' is invalid");
            }
        }
        WebSocketMessage::SomeoneLeftAudioChannel { data } => {
            tracing::info!(
                "User {} left audio channel {} on server {}",
                data.user.username,
                data.channel.id,
                data.channel.server_id
            );
            let handle = handle;
            if let Some(audio_element) = audio {
                if let Err(e) = audio_element.handle_leave_channel(&data) {
                    tracing::error!("Failed to handle leave channel: {}", e);
                }
            }
            // Fails only when the event name is invalid
            if handle.emit("someone-left-audio-channel", data).is_err() {
                tracing::error!("Event name 'someone-left-audio-channel' is invalid");
            }
        }
    }
    Ok(())
}
