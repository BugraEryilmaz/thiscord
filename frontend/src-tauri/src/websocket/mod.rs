use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use cpal::traits::HostTrait;
use futures_util::{SinkExt, StreamExt};
use shared::{Split, WebRTCConnection, WebSocketMessage};
use native_tls::TlsConnector;
use reqwest::cookie::CookieStore;
use reqwest::header;
use ringbuf::HeapRb;
use front_shared::URL;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::Sender;
use tokio::{select, sync::mpsc::Receiver};
use tokio_tungstenite::{
    connect_async_tls_with_config, tungstenite::client::IntoClientRequest,
    tungstenite::Message::Text, Connector,
};
use uuid::Uuid;

pub enum WebSocketRequest {
    JoinAudioChannel { server_id: Uuid, channel_id: Uuid },
    DisconnectFromAudioChannel,
    Disconnect,
}

use crate::{utils::AppState, Error};

pub async fn websocket_handler(
    handle: AppHandle,
    cmd_rx: &mut Receiver<WebSocketRequest>,
) -> Result<(), Error> {
    let state = handle.state::<AppState>();
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel(100);
    let url = format!("wss://{}/websocket", URL);
    let mut web_rtc_connection: Option<WebRTCConnection> = None;

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
                        if let Err(e) = handle_websocket_message(message, &mut web_rtc_connection, ws_tx.clone(), &state).await {
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
                        handle_internal_request(message, &mut web_rtc_connection, ws_tx.clone(), &state).await
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
    Ok(())
}

pub async fn handle_internal_request(
    request: WebSocketRequest,
    web_rtc_connection: &mut Option<WebRTCConnection>,
    socket: Sender<WebSocketMessage>,
    state: &AppState,
) -> Result<(), Error> {
    match request {
        WebSocketRequest::JoinAudioChannel {
            server_id,
            channel_id,
        } => {
            *web_rtc_connection = Some(WebRTCConnection::new().await?);
            let web_rtc_connection = web_rtc_connection.as_ref().unwrap();
            let audio_element = &state.audio_element;
            let host = cpal::default_host();
            let input_device = host
                .default_input_device()
                .ok_or_else(|| Error::NoInputDevice)?;
            let output_device = host
                .default_output_device()
                .ok_or_else(|| Error::NoOutputDevice)?;

            // Start the audio element streams
            let dropped = Arc::new(AtomicBool::new(false));
            let (speaker_producers, speaker_consumers): (Vec<_>, Vec<_>) =
                (1..10).map(|_| HeapRb::<i16>::new(12000).split()).unzip();

            let mic_consumer = audio_element.start_input_stream(input_device, dropped.clone())?;
            audio_element.start_output_stream(output_device, speaker_consumers)?;

            // Create the WebRTC streams
            let audio_track = web_rtc_connection.create_audio_track_sample(10).await?;
            let audio_track = audio_track[0].clone();
            web_rtc_connection
                .background_stream_audio(mic_consumer, dropped, audio_track)
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
            web_rtc_connection.take();
            let audio_element = &state.audio_element;
            audio_element.quit()?;
            let disconnect_message = WebSocketMessage::DisconnectFromAudioChannel;
            if let Err(e) = socket.send(disconnect_message).await {
                tracing::error!("Failed to send disconnect audio channel message: {}", e);
            }
        }
        WebSocketRequest::Disconnect => todo!(),
    }
    Ok(())
}

pub async fn handle_websocket_message(
    message: WebSocketMessage,
    web_rtc_connection: &mut Option<WebRTCConnection>,
    tx: Sender<WebSocketMessage>,
    _state: &AppState,
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
    }
    Ok(())
}
