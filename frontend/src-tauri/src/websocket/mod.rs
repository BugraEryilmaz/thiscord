use std::sync::{Arc, Mutex as StdMutex};

use futures_util::{SinkExt, StreamExt};
use shared::{HeapCons, RTCPeerConnectionState, WebRTCConnection, WebSocketMessage, ROOM_SIZE};
use native_tls::TlsConnector;
use reqwest::cookie::CookieStore;
use reqwest::header;
use ringbuf::HeapProd;
use front_shared::{Status, URL};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::Sender;
use tokio::{select, sync::mpsc::Receiver};
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::client::IntoClientRequest,
    tungstenite::Message::Text, Connector,
};
use uuid::Uuid;

pub enum WebSocketRequest {
    JoinAudioChannel { server_id: Uuid, channel_id: Uuid, channel_name: String },
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
    
    if let Some(cookies) = cookie_store.cookies(&reqwest::Url::parse(format!("https://{}", URL).as_str()).unwrap()) {
        request
            .headers_mut()
            .insert(header::COOKIE, cookies);
    }

    let connector = TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
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
        WebSocketRequest::JoinAudioChannel {
            server_id,
            channel_id,
            channel_name,
        } => {
            *web_rtc_connection = Some(WebRTCConnection::new(channel_id).await?);
            let web_rtc_connection = web_rtc_connection.as_ref().unwrap();
            *audio = Some(AudioElement::new(handle.clone()));
            let audio_element = audio.as_mut().unwrap();

            // Start the audio element streams
            let mic_consumer: Arc<StdMutex<HeapCons<f32>>> = audio_element.start_mic()?;
            let speaker_producers: Vec<Arc<StdMutex<HeapProd<f32>>>> = audio_element.start_speaker()?;

            // Create the WebRTC streams
            let audio_track = web_rtc_connection.create_audio_track_sample(ROOM_SIZE).await?;
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
            web_rtc_connection
                .peer_connection
                .on_peer_connection_state_change(Box::new(move |state| {
                    tracing::debug!("Peer connection state: {:?}", state);
                    let appstate = handle.state::<AppState>();
                    if state == RTCPeerConnectionState::Connected {
                        appstate.change_status(Status::OnCall(channel_name.clone()), &handle);
                    }
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
                }
            } 
            match &command {
                AudioCommand::SetMic(device_name) => {
                    AudioElement::set_default_mic(device_name, handle);
                }
                AudioCommand::SetSpeaker(device_name) => {
                    AudioElement::set_default_speaker(device_name, handle);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

pub async fn handle_websocket_message(
    message: WebSocketMessage,
    web_rtc_connection: &mut Option<WebRTCConnection>,
    _audio: &mut Option<AudioElement>,
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
        },
        WebSocketMessage::WebRTCAnswer(_) => {
            tracing::warn!("Received WebRTCAnswer message, but this is client");
        },
        WebSocketMessage::Disconnect => {
            tracing::info!("Received Disconnect message, but this is client");
        },
        WebSocketMessage::Error { err } => {
            tracing::error!("WebSocket error: {}", err);
            return Err(Error::WebSocketError(err));
        },
        WebSocketMessage::SomeoneJoinedAudioChannel { data } => {
            tracing::info!("User {} joined audio channel {} on server {}", data.user.username, data.channel.id, data.channel.server_id);
            let handle = handle;
            // Fails only when the event name is invalid
            if handle.emit("someone-joined-audio-channel", data).is_err() {
                tracing::error!("Event name 'someone-joined-audio-channel' is invalid");
            }
        }
        WebSocketMessage::SomeoneLeftAudioChannel { data } => {
            tracing::info!("User {} left audio channel {} on server {}", data.user.username, data.channel.id, data.channel.server_id);
            let handle = handle;
            // Fails only when the event name is invalid
            if handle.emit("someone-left-audio-channel", data).is_err() {
                tracing::error!("Event name 'someone-left-audio-channel' is invalid");
            }
        }
    }
    Ok(())
}
