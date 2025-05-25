use ringbuf::HeapCons;
use ringbuf::HeapProd;
use ringbuf::traits::Consumer;
use ringbuf::traits::Observer;
use ringbuf::traits::Producer;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice::udp_network::EphemeralUDP;
use webrtc::ice::udp_network::UDPNetwork;
use std::sync::Arc;

use crate::{Error, Reader, SignalingMessage, Writer};

use futures_util::StreamExt;
use native_tls::TlsConnector;
use opus::{Application, Channels};
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
    pub audio_track: Arc<TrackLocalStaticSample>,
    pub audio_config: AudioConfig,
    pub ws_writer: Mutex<Option<Arc<Mutex<Writer>>>>,
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
        let audio_track = Self::create_audio_track();
        peer_connection.add_track(audio_track.clone()).await?;
        Ok(WebRTCConnection {
            peer_connection: peer_connection,
            audio_track: audio_track,
            audio_config: AudioConfig::default(),
            ws_writer: Mutex::new(None),
        })
    }

    pub async fn new_with_writer(ws_write: Arc<Mutex<Writer>>) -> Result<Self, Error> {
        let peer_connection = Self::create_peer_connection().await?;
        let audio_track = Self::create_audio_track();
        peer_connection.add_track(audio_track.clone()).await?;
        Ok(WebRTCConnection {
            peer_connection: peer_connection,
            audio_track: audio_track,
            audio_config: AudioConfig::default(),
            ws_writer: Mutex::new(Some(ws_write)),
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
        *ws_writer_guard = Some(Arc::new(Mutex::new(Writer::Client(ws_writer))));
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
                        self_clone
                            .handle_signaling_message(message)
                            .await
                            .unwrap_or_else(|e| {
                                eprintln!("Error handling signaling message: {}", e);
                            });
                    }
                    Err(e) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    Ok(None) => continue,
                }
            }
        });
        Ok(())
    }

    pub async fn handle_signaling_message(&self, message: SignalingMessage) -> Result<(), Error> {
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
        }
        Ok(())
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
            bundle_policy: RTCBundlePolicy::MaxBundle,
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

    pub fn create_audio_track() -> Arc<TrackLocalStaticSample> {
        Arc::new(TrackLocalStaticSample::new(
            Self::get_audio_codec().capability,
            "audio".to_string(),
            "thiscord".to_string(),
        ))
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

    pub async fn background_stream_audio(&self, mut data: HeapCons<i16>) -> Result<(), Error> {
        let audio_track = self.audio_track.clone();
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
                    println!("popped only {} samples, waiting for more", read_len);
                    continue;
                }
                let mut encoded = vec![0u8; opus_max_payload_size];
                let encoded_bytes =
                    opus_encoder
                        .encode(&buffer, &mut encoded)
                        .unwrap_or_else(|e| {
                            eprintln!("Opus encoding error: {}", e);
                            0
                        });
                if encoded_bytes > 0 {
                    let sample = Sample {
                        data: encoded[..encoded_bytes].to_vec().into(),
                        duration: std::time::Duration::from_millis(20), // 20ms
                        ..Default::default()
                    };
                    let writer = audio_track.sample_writer();
                    if let Err(e) = writer.write_sample(&sample).await {
                        eprintln!("Error writing sample: {}", e);
                    } else {
                        // println!("Sent audio frame");
                    }
                } else {
                    eprintln!("No encoded bytes");
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
        let ws_writer_guard = self.ws_writer.lock().await;
        let ws_writer = ws_writer_guard
            .clone()
            .ok_or(Error::WebSocketNotConnected)?;
        let message = SignalingMessage::Offer(RTCSessionDescription::offer(offer)?);
        let mut writer = ws_writer.lock().await;
        writer.send(message).await?;
        println!("Offer sent to remote peer");
        Ok(())
    }

    pub async fn answer(&self, sdp: String) -> Result<(), Error> {
        eprintln!("Answering to SDP: {}", sdp);
        let remote_sdp = RTCSessionDescription::offer(sdp)?;
        eprintln!("Parsed remote SDP: {:?}", remote_sdp);
        self.peer_connection
            .set_remote_description(remote_sdp)
            .await?;
        eprintln!("Remote description set successfully");
        // Create an answer
        let answer = self
            .peer_connection
            .create_answer(Some(RTCAnswerOptions {
                voice_activity_detection: true,
            }))
            .await?;
        eprintln!("Created answer: {:?}", answer);
        // Set the local description
        self.peer_connection
            .set_local_description(answer.clone())
            .await?;
        eprintln!("Local description set successfully");
        // Send the answer back to the remote peer
        let ws_writer = self
            .ws_writer
            .lock()
            .await
            .clone()
            .ok_or(Error::WebSocketNotConnected)?;
        let message = SignalingMessage::Answer(answer);
        let mut writer = ws_writer.lock().await;
        writer.send(message).await?;
        eprintln!("Answer sent to remote peer");
        self.setup_ice_handling().await?;
        Ok(())
    }

    pub async fn background_receive_audio(&self, data: HeapProd<i16>) -> Result<(), Error> {
        let audio_config = self.audio_config.clone();
        println!("Setting up background receive audio");
        // Wrap data in Arc<Mutex<...>> to make it thread-safe for the closure
        let data = std::sync::Arc::new(tokio::sync::Mutex::new(data));
        self.peer_connection.on_track(Box::new({
            let data = data.clone();
            move |track, _receiver, _| {
                println!("Received remote track: {}", track.kind());

                if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                    let data = data.clone();
                    tokio::spawn(async move {
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
                                    eprintln!("Opus decoding error: {}", e);
                                    0
                                });
                            if decoded_bytes == 0 {
                                eprintln!("No decoded bytes");
                                continue;
                            }
                            let mut data_guard = data.lock().await;
                            data_guard.push_slice(&decoded[..decoded_bytes]);
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
        let ws_writer = self
            .ws_writer
            .lock()
            .await
            .clone()
            .ok_or(Error::WebSocketNotConnected)?;
        self.peer_connection
            .on_ice_candidate(Box::new(move |candidate| {
                let ws_writer = ws_writer.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        // Send this candidate to the remote peer via your signaling channel
                        // You MUST implement this part!
                        match Self::send_ice_candidate_to_remote_peer(candidate, ws_writer.clone())
                            .await
                        {
                            Ok(()) => {}
                            Err(e) => {
                                eprintln!("Failed to send ICE candidate: {}", e);
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
        ws_writer: Arc<Mutex<Writer>>,
    ) -> Result<(), Error> {
        let candidate_init = candidate.to_json()?;
        let message = SignalingMessage::IceCandidate(candidate_init);
        let mut writer = ws_writer.lock().await;
        writer.send(message).await?;
        Ok(())
    }

    pub async fn add_remote_ice_candidate(
        &self,
        candidate: RTCIceCandidateInit,
    ) -> Result<(), Error> {
        self.peer_connection.add_ice_candidate(candidate).await?;
        Ok(())
    }
}
