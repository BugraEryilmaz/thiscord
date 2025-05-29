use ringbuf::HeapCons;
use ringbuf::HeapRb;
use ringbuf::traits::Consumer;
use ringbuf::traits::Observer;
use ringbuf::traits::Producer;
use ringbuf::traits::Split;
use std::sync::Arc;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice::udp_network::EphemeralUDP;
use webrtc::ice::udp_network::UDPNetwork;
use webrtc::rtp::packet::Packet;
use webrtc::track::track_local::TrackLocalWriter;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

use crate::IsClosed;
use crate::{Error, Reader, SignalingMessage, Writer};

use futures_util::StreamExt;
use native_tls::TlsConnector;
use opus::{Application, Channels};
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex;
use tokio_tungstenite::Connector;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use webrtc::api::media_engine::MIME_TYPE_OPUS;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
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
    pub ws_writer: Arc<Mutex<Option<Writer>>>,
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
    pub async fn new() -> Result<Self, Error> {
        let peer_connection = Self::create_peer_connection().await?;
        Ok(WebRTCConnection {
            peer_connection: peer_connection,
            audio_config: AudioConfig::default(),
            ws_writer: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn new_with_writer(ws_write: Writer) -> Result<Self, Error> {
        let peer_connection = Self::create_peer_connection().await?;
        Ok(WebRTCConnection {
            peer_connection: peer_connection,
            audio_config: AudioConfig::default(),
            ws_writer: Arc::new(Mutex::new(Some(ws_write))),
        })
    }

    pub async fn connect_ws(self: Arc<Self>, url: &str) -> Result<(), Error> {
        // Create a WebSocket connection to the signaling server using tokio-tungstenite

        let request = url.into_client_request()?;
        let connector = TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let connector = Connector::NativeTls(connector);
        let (ws_stream, _) =
            connect_async_tls_with_config(request, None, false, Some(connector)).await?;
        let (ws_writer, ws_reader) = ws_stream.split();
        let mut ws_writer_guard = self.ws_writer.lock().await;
        *ws_writer_guard = Some(Writer::Client(ws_writer));
        let self_clone: Arc<WebRTCConnection> = Arc::clone(&self);

        // Handle incoming messages
        self_clone.create_handler(Reader::Client(ws_reader)).await?;

        Ok(())
    }

    pub async fn create_handler(self: Arc<Self>, mut ws_reader: Reader) -> Result<(), Error> {
        let self_clone: Arc<WebRTCConnection> = Arc::clone(&self);

        // Handle incoming messages
        tokio::spawn(async move {
            loop {
                match ws_reader.next().await {
                    Ok(Some(message)) => {
                        let closed = self_clone
                            .handle_signaling_message(message)
                            .await
                            .unwrap_or_else(|e| {
                                tracing::error!("Error handling signaling message: {}, Closing connection", e);
                                IsClosed::NotClosed
                            });
                        if closed == IsClosed::Closed {
                            self.close().await;
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("WebSocket error: {}", e);
                        break;
                    }
                    Ok(None) => continue,
                }
            }
        });
        Ok(())
    }

    pub async fn handle_signaling_message(
        &self,
        message: SignalingMessage,
    ) -> Result<IsClosed, Error> {
        match message {
            SignalingMessage::Offer(offer) => {
                // Handle incoming offer
                self.answer(offer.sdp).await?;
            }
            SignalingMessage::Answer(answer) => {
                // Handle incoming answer
                self.peer_connection.set_remote_description(answer).await?;
            }
            SignalingMessage::IceCandidate(candidate_init) => {
                // Handle incoming ICE candidate
                self.add_remote_ice_candidate(candidate_init).await?;
            }
            SignalingMessage::Close => {
                // Handle close message
                tracing::info!("Received close message, closing WebRTC connection");
                return Ok(IsClosed::Closed);
            }
        }
        Ok(IsClosed::NotClosed)
    }

    pub async fn create_peer_connection() -> Result<RTCPeerConnection, Error> {
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

        let config = Self::get_config();
        let peer_connection = api.new_peer_connection(config).await.map_err(|e| e.into());
        peer_connection
    }

    pub fn get_config() -> RTCConfiguration {
        let config = RTCConfiguration {
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

    pub async fn create_offer(&self) -> Result<String, Error> {
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
        let sdp = offer.sdp;
        Ok(sdp)
    }

    pub async fn background_stream_audio(
        &self,
        mut data: HeapCons<i16>,
        audio_tracks: Vec<Arc<Mutex<Option<Arc<TrackLocalStaticSample>>>>>,
    ) -> Result<(), Error> {
        // Opus frames typically encode 20ms of audio
        let channels = self.audio_config.channels;
        let frame_size = self.audio_config.frame_size;
        let opus_max_payload_size = self.audio_config.opus_max_payload_size;
        let mut opus_encoder = self.audio_config.get_opus_encoder()?;

        tokio::spawn(async move {
            loop {
                if data.occupied_len() < frame_size * (channels as usize) {
                    // Wait for enough data to fill a frame
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
                // Pop data from the ring buffer
                let mut buffer = vec![0i16; frame_size * (channels as usize)];
                let read_len = data.pop_slice(buffer.as_mut_slice());
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
                        .encode(&buffer, &mut encoded)
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
                    for audio_track in &audio_tracks {
                        let audio_track = audio_track.lock().await;
                        if let Some(track) = audio_track.as_ref() {
                            if let Err(e) = track.write_sample(&sample).await {
                                tracing::error!("Error writing audio sample: {}", e);
                            } else {
                                tracing::trace!("Sent audio frame: {:?}", sample);
                            }
                        }
                    }
                } else {
                    eprintln!("No encoded bytes");
                }
            }
        });
        Ok(())
    }

    pub async fn background_stream_data(
        &self,
        data: Arc<Mutex<Option<HeapCons<Packet>>>>,
        audio_tracks: Vec<Arc<Mutex<Option<Arc<TrackLocalStaticRTP>>>>>,
    ) -> Result<(), Error> {
        tokio::spawn(async move {
            loop {
                // Pop data from the ring buffer
                let mut data_guard = data.lock().await;
                if data_guard.is_none() {
                    drop(data_guard);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
                if let Some(packet) = data_guard.as_mut().unwrap().try_pop() {
                    drop(data_guard);
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
                    drop(data_guard);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
            }
        });
        Ok(())
    }

    pub async fn offer(&self) -> Result<(), Error> {
        let offer = self.create_offer().await?;
        self.peer_connection
            .set_local_description(RTCSessionDescription::offer(offer.clone())?)
            .await?;
        let mut ws_writer_guard = self.ws_writer.lock().await;
        match ws_writer_guard.as_mut() {
            Some(writer) => {
                let message = RTCSessionDescription::offer(offer)?;
                writer.send(SignalingMessage::Offer(message)).await?;
                Ok(())
            }
            None => Err(Error::WebSocketNotConnected),
        }
    }

    pub async fn answer(&self, sdp: String) -> Result<(), Error> {
        let remote_sdp = RTCSessionDescription::offer(sdp)?;
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
        let message = SignalingMessage::Answer(answer);
        let mut ws_writer = self.ws_writer.lock().await;
        match ws_writer.as_mut() {
            Some(writer) => {
                tracing::debug!("Sending answer: {:?}", message);
                writer.send(message).await?;
                tracing::info!("Sent answer to remote peer");
            }
            None => return Err(Error::WebSocketNotConnected),
        }
        drop(ws_writer);
        self.setup_ice_handling().await?;
        Ok(())
    }

    pub async fn background_receive_audio(
        &self,
        receiver_queues: Arc<StdMutex<Vec<HeapCons<i16>>>>,
    ) -> Result<(), Error> {
        let audio_config = self.audio_config.clone();
        tracing::info!("Setting up background receive audio");

        self.peer_connection.on_track(Box::new({
            // let receiver_queues = receiver_queues.clone();
            move |track, _receiver, _| {
                tracing::info!("Received remote track: {}", track.kind());

                if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                    let (tx, rx) = HeapRb::<i16>::new(12000).split();
                    let data = Arc::new(Mutex::new(tx));
                    let receiver_queues = receiver_queues.clone();
                    let mut receiver_queues_guard = receiver_queues
                        .lock()
                        .expect("Failed to lock receiver queues");
                    receiver_queues_guard.push(rx);
                    drop(receiver_queues_guard);
                    tokio::spawn(async move {
                        // let track_map = Arc::clone(&track_map);
                        let mut opus_decoder = audio_config.get_opus_decoder().unwrap();
                        while let Ok((rtp, _)) = track.read_rtp().await {
                            let mut decoded = vec![
                                0i16;
                                audio_config.frame_size
                                    * (audio_config.channels as usize)
                            ];
                            let decoded_bytes = opus_decoder
                                .decode(&rtp.payload, &mut decoded, false)
                                .unwrap_or_else(|e| {
                                    tracing::error!("Opus decoding error: {}", e);
                                    0
                                });
                            if decoded_bytes == 0 {
                                tracing::warn!("No decoded bytes, skipping");
                                continue;
                            }
                            let mut data = data.lock().await;

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

    pub async fn background_receive_data(
        &self,
        receiver_queue: Arc<Mutex<Option<HeapCons<Packet>>>>,
    ) -> Result<(), Error> {
        tracing::info!("Setting up background receive data");

        self.peer_connection.on_track(Box::new({
            move |track, _receiver, _| {
                tracing::info!("Received remote track: {}", track.kind());
                if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                    let receiver_queue = Arc::clone(&receiver_queue);
                    let (tx, rx) = HeapRb::<Packet>::new(10).split();
                    let data = Mutex::new(tx);
                    tokio::spawn(async move {
                        let mut receiver_queues_guard = receiver_queue.lock().await;
                        *receiver_queues_guard = Some(rx);
                        drop(receiver_queues_guard);
                        while let Ok((rtp, _)) = track.read_rtp().await {
                            let mut data = data.lock().await;

                            match data.try_push(rtp) {
                                Ok(_) => {
                                    tracing::trace!("Pushed packet to data");
                                }
                                Err(e) => {
                                    tracing::error!("Failed to push packet to data: {}", e);
                                }
                            }
                        }
                    });
                }

                Box::pin(async {})
            }
        }));
        Ok(())
    }

    pub async fn setup_ice_handling(&self) -> Result<(), Error> {
        // Set up ICE candidate handler
        let ws_writer = self.ws_writer.clone();
        self.peer_connection
            .on_ice_candidate(Box::new(move |candidate| {
                let ws_writer = ws_writer.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        // Send this candidate to the remote peer via your signaling channel
                        match Self::send_ice_candidate_to_remote_peer(candidate, ws_writer).await {
                            Ok(()) => {}
                            Err(e) => {
                                tracing::error!("Failed to send ICE candidate: {}", e);
                            }
                        }
                    }
                })
            }));

        // Handle remote ICE candidates
        self.peer_connection
            .on_ice_connection_state_change(Box::new(move |state| {
                println!("ICE connection state: {:?}", state);
                Box::pin(async {})
            }));

        Ok(())
    }

    pub async fn send_ice_candidate_to_remote_peer(
        candidate: RTCIceCandidate,
        ws_writer: Arc<Mutex<Option<Writer>>>,
    ) -> Result<(), Error> {
        tracing::debug!("Sending ICE candidate");
        let candidate_init = candidate.to_json()?;
        let message = SignalingMessage::IceCandidate(candidate_init);
        let mut writer = ws_writer.lock().await;
        if writer.is_none() {
            return Err(Error::WebSocketNotConnected);
        }
        let writer = writer.as_mut().unwrap();
        writer.send(message).await?;
        Ok(())
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
        let mut ws_writer_guard = self.ws_writer.lock().await;
        if let Some(writer) = ws_writer_guard.as_mut() {
            writer.send(SignalingMessage::Close).await.err();
        }
        ws_writer_guard.take();
        drop(ws_writer_guard);
        self.peer_connection.close().await.err();
        tracing::info!("WebRTC connection closed");
    }
}
