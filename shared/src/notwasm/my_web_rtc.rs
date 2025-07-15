use ringbuf::{HeapCons, HeapProd, traits::{Consumer, Observer, Producer}};
use uuid::Uuid;
use std::sync::{Arc, Mutex as StdMutex};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice::udp_network::EphemeralUDP;
use webrtc::ice::udp_network::UDPNetwork;
use webrtc::rtp::packet::Packet;
use webrtc::track::track_local::TrackLocalWriter;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

use crate::{models::TurnCreds, Error};
use crate::WebSocketMessage;

use opus::{Application, Channels};
use tokio::sync::Mutex;
use webrtc::api::media_engine::MIME_TYPE_OPUS;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::offer_answer_options::{RTCAnswerOptions, RTCOfferOptions};
use webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTPCodecType};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::{
    api::media_engine::MediaEngine, peer_connection::RTCPeerConnection,
    rtp_transceiver::rtp_codec::RTCRtpCodecParameters,
};

pub struct WebRTCConnection {
    pub peer_connection: RTCPeerConnection,
    pub audio_config: AudioConfig,
    pub room_id: Uuid,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: Channels,
    pub frame_size: usize,
    pub opus_max_payload_size: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            sample_rate: 48000,
            channels: Channels::Mono,
            frame_size: 960, // 20ms at 48kHz
            opus_max_payload_size: 1275,
        }
    }
}

impl AudioConfig {
    pub fn get_opus_encoder(&self) -> Result<opus::Encoder, opus::Error> {
        opus::Encoder::new(self.sample_rate, self.channels, Application::Audio)
    }

    pub fn get_opus_decoder(&self) -> Result<opus::Decoder, opus::Error> {
        opus::Decoder::new(self.sample_rate, self.channels)
    }
}

impl WebRTCConnection {
    pub async fn new(room_id: Uuid, turn_creds: Option<TurnCreds>) -> Result<Self, Error> {
        let peer_connection = Self::create_peer_connection(turn_creds).await?;
        Ok(WebRTCConnection {
            peer_connection: peer_connection,
            audio_config: AudioConfig::default(),
            room_id,
        })
    }

    pub async fn create_peer_connection(turn_creds: Option<TurnCreds>) -> Result<RTCPeerConnection, Error> {
        let mut m = MediaEngine::default();
        m.register_codec(Self::get_audio_codec(), RTPCodecType::Audio)?;

        let mut udp = EphemeralUDP::default();
        udp.set_ports(12000, 13000)?;

        let mut settings_engine = SettingEngine::default();
        settings_engine.set_udp_network(UDPNetwork::Ephemeral(udp));

        let api = webrtc::api::APIBuilder::new()
            .with_media_engine(m)
            .with_setting_engine(settings_engine)
            .build();

        let config = Self::get_config(turn_creds);
        let peer_connection = api.new_peer_connection(config).await.map_err(|e| e.into());
        peer_connection
    }

    pub fn get_config(turn_creds: Option<TurnCreds>) -> RTCConfiguration {
        let mut config = RTCConfiguration {
            ice_servers: vec![
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                    ..Default::default()
                },
                // Optional TURN server:
                // RTCIceServer {
                //     urls: vec!["turn:turn.example.com:3478".to_owned()],
                //     username: "user".to_string(),
                //     credential: "pass".to_string(),
                //     ..Default::default()
                // },
            ],
            bundle_policy: RTCBundlePolicy::Balanced,
            ice_candidate_pool_size: 2,
            ..Default::default()
        };
        if let Some(turn_creds) = turn_creds {
            config.ice_servers.push(RTCIceServer {
                urls: vec![
                    format!("turns:{}:5349", turn_creds.realm),
                    format!("turns:{}:5350", turn_creds.realm),
                ],
                username: turn_creds.username,
                credential: turn_creds.credential,
            });
        }

        config
    }

    pub fn get_audio_codec() -> RTCRtpCodecParameters {
        let audio_codec = RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 1,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 111,
            ..Default::default()
        };
        audio_codec
    }

    pub async fn create_audio_track_sample(
        &self,
        n: usize,
    ) -> Result<Vec<Arc<TrackLocalStaticSample>>, Error> {
        let mut ret = Vec::new();
        for id in 0..n {
            let track = Arc::new(TrackLocalStaticSample::new(
                Self::get_audio_codec().capability,
                format!("client-audio-{}", id),
                format!("client-audio-stream-{}", id),
            ));
            self.peer_connection.add_track(track.clone()).await?;
            ret.push(track);
        }
        Ok(ret)
    }

    pub async fn create_audio_track_rtp(
        &self,
        n: usize,
    ) -> Result<Vec<Arc<TrackLocalStaticRTP>>, Error> {
        let mut ret = Vec::new();
        for id in 0..n {
            let track = Arc::new(TrackLocalStaticRTP::new(
                Self::get_audio_codec().capability,
                format!("server-audio-{}", id),
                format!("server-audio-stream-{}", id),
            ));
            self.peer_connection.add_track(track.clone()).await?;
            ret.push(track);
        }
        Ok(ret)
    }

    pub async fn create_offer(&self) -> Result<WebSocketMessage, Error> {
        let offer = self
            .peer_connection
            .create_offer(Some(RTCOfferOptions {
                voice_activity_detection: true,
                ice_restart: false,
            }))
            .await?;
        // Set the local description
        self.peer_connection
            .set_local_description(offer.clone())
            .await?;
        Ok(WebSocketMessage::WebRTCOffer(offer))
    }

    pub async fn background_stream_audio(
        &self,
        data: Arc<StdMutex<HeapCons<f32>>>,
        audio_tracks: Arc<TrackLocalStaticSample>,
    ) -> Result<(), Error> {
        // Opus frames typically encode 20ms of audio
        let channels = self.audio_config.channels;
        let frame_size = self.audio_config.frame_size;
        let opus_max_payload_size = self.audio_config.opus_max_payload_size;
        let mut opus_encoder = self.audio_config.get_opus_encoder()?;

        tokio::spawn(async move {
            loop {
                if data.lock().unwrap().occupied_len() < frame_size * (channels as usize) {
                    // Wait for enough data to fill a frame
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
                // Pop data from the ring buffer
                let mut buffer = vec![0f32; frame_size * (channels as usize)];
                let read_len = {
                    data.lock().unwrap().pop_slice(buffer.as_mut_slice())
                };
                if read_len < frame_size * (channels as usize) {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    tracing::warn!(
                        "Not enough samples to encode, expected {}, got {}",
                        frame_size * (channels as usize),
                        read_len
                    );
                    continue;
                }
                let mut encoded = vec![0u8; opus_max_payload_size];
                let encoded_bytes =
                    opus_encoder
                        .encode_float(&buffer, &mut encoded)
                        .unwrap_or_else(|e| {
                            tracing::error!("Opus encoding error: {}", e);
                            0
                        });
                if encoded_bytes > 0 {
                    let sample = Sample {
                        data: encoded[..encoded_bytes].to_vec().into(),
                        duration: std::time::Duration::from_millis(20), // 20ms
                        ..Default::default()
                    };
                    if let Err(e) = audio_tracks.write_sample(&sample).await {
                        tracing::error!("Error writing audio sample: {}", e);
                    } else {
                        tracing::trace!("Sent audio frame: {:?}", sample);
                    }
                } else {
                    eprintln!("No encoded bytes");
                }
            }
        });
        Ok(())
    }

    pub fn background_stream_data(
        &self,
        mut data: HeapCons<Packet>,
        dropped: Arc<AtomicBool>,
        audio_tracks: Vec<Arc<Mutex<Option<Arc<TrackLocalStaticRTP>>>>>,
    ) {
        tokio::spawn(async move {
            loop {
                if dropped.load(Ordering::Relaxed) {
                    tracing::info!("Data stream dropped, exiting background task");
                    break;
                }
                // Pop data from the ring buffer
                if let Some(packet) = data.try_pop() {
                    for audio_track in &audio_tracks {
                        let audio_track = audio_track.lock().await;
                        if let Some(audio_track) = audio_track.as_ref() {
                            if let Err(e) = audio_track.write_rtp(&packet).await {
                                tracing::error!("Error writing RTP packet: {}", e);
                            } else {
                                tracing::trace!("Sent RTP packet: {:?}", packet);
                            }
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
            }
        });
    }

    pub async fn create_answer(&self, remote_sdp: RTCSessionDescription) -> Result<WebSocketMessage, Error> {
        tracing::debug!("Setting remote description: {:?}", remote_sdp);
        self.peer_connection
            .set_remote_description(remote_sdp)
            .await?;
        tracing::info!("Remote description set successfully");
        // Create an answer
        let answer = self
            .peer_connection
            .create_answer(Some(RTCAnswerOptions {
                voice_activity_detection: true,
            }))
            .await?;
        tracing::debug!("Created answer: {:?}", answer);
        // Set the local description
        self.peer_connection
            .set_local_description(answer.clone())
            .await?;
        tracing::info!("Local description set successfully");
        // Send the answer back to the remote peer
        Ok(WebSocketMessage::WebRTCAnswer(answer))
    }

    pub async fn background_receive_audio(
        &self,
        receiver_queues: Vec<Arc<StdMutex<HeapProd<f32>>>>,
    ) -> Result<(), Error> {
        let audio_config = self.audio_config.clone();
        tracing::info!("Setting up background receive audio");
        // let data = receiver_queues.into_iter().map(|q| Arc::new(Mutex::new(q))).collect::<Vec<_>>();
        let data = receiver_queues;

        self.peer_connection.on_track(Box::new({
            // let receiver_queues = receiver_queues.clone();
            move |track, _receiver, _| {
                tracing::info!("Received remote track: {}", track.kind());
                println!("Track ID: {}", track.id());

                if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                    // track_id = server-audio-{id}
                    let track_id = track.id();
                    let id = track_id.split('-').last().unwrap_or("0");
                    let id = id.parse::<usize>().unwrap_or(0);
                    let data = data[id].clone();
                    return Box::pin(async move {
                        // let track_map = Arc::clone(&track_map);
                        let mut opus_decoder = audio_config.get_opus_decoder().unwrap();
                        while let Ok((rtp, _)) = track.read_rtp().await {
                            let mut decoded = vec![
                                0f32;
                                audio_config.frame_size
                                    * (audio_config.channels as usize)
                            ];
                            let decoded_bytes = opus_decoder
                                .decode_float(&rtp.payload, &mut decoded, false)
                                .unwrap_or_else(|e| {
                                    tracing::error!("Opus decoding error: {}", e);
                                    0
                                });
                            if decoded_bytes == 0 {
                                tracing::warn!("No decoded bytes, skipping");
                                continue;
                            }
                            let mut data = data.lock().unwrap();

                            // let mut data_guard = data.lock().await;
                            data.push_slice(&decoded[..decoded_bytes]);
                        }
                    });
                }

                Box::pin(async {})
            }
        }));
        Ok(())
    }

    pub fn background_receive_data(
        &self,
        receiver_queue: Arc<Mutex<HeapProd<Packet>>>,
        dropped: Arc<AtomicBool>,
    ) {
        tracing::info!("Setting up background receive data");

        self.peer_connection.on_track(Box::new({
            move |track, _receiver, _| {
                println!("Track ID: {}", track.id());
                tracing::info!("Received remote track: {}", track.kind());
                if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                    let receiver_queue = receiver_queue.clone();
                    let dropped = dropped.clone();
                    return Box::pin(async move {
                        let mut receiver_queue = receiver_queue.lock().await;
                        while let Ok((rtp, _)) = track.read_rtp().await {
                            match receiver_queue.try_push(rtp) {
                                Ok(_) => {
                                    tracing::trace!("Pushed packet to data");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to push packet to data: {}", e);
                                }
                            }
                        }
                        dropped.store(true, Ordering::Relaxed);
                        tracing::info!("Track closed, setting dropped to true");
                    });
                }

                Box::pin(async {})
            }
        }));
    }

    /**
     * Sets up ICE handling for the WebRTC connection.
     * Callback should send the candidate to the remote peer via your signaling channel.
     */
    pub fn setup_ice_handling<F, Fut>(&self, callback: F)
    where
        F: Fn(WebSocketMessage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        // Wrap the callback in an Arc to allow it to be cloned into the async block
        let callback = Arc::new(callback);

        self.peer_connection.on_ice_candidate(Box::new({
            let callback = callback.clone();
            move |candidate| {
                let callback = callback.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        let candidate = match candidate.to_json() {
                            Ok(json) => json,
                            Err(err) => {
                                tracing::error!("Failed to serialize ICE candidate: {}", err);
                                return;
                            }
                        };
                        let msg = WebSocketMessage::IceCandidate(candidate);

                        (callback)(msg).await;
                    }
                })
            }
        }));

        // Handle remote ICE candidates
        self.peer_connection
            .on_ice_connection_state_change(Box::new(move |state| {
                tracing::debug!("ICE connection state: {:?}", state);
                Box::pin(async {})
            }));
    }

    pub async fn add_remote_ice_candidate(
        &self,
        candidate: RTCIceCandidateInit,
    ) -> Result<(), Error> {
        tracing::debug!("Adding remote ICE candidate");
        self.peer_connection.add_ice_candidate(candidate).await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.peer_connection.close().await.err();
        tracing::info!("WebRTC connection closed");
    }
}
