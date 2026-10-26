//! Compressed-video bridge to the WebView's WebRTC video decoder/compositor.
//! Viewer peers bind only loopback, use no STUN/TURN and never carry audio.
use super::bridge::Recorder;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use webrtc::{
    media_stream::track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
    peer_connection::*,
};

struct Handler {
    gathered: Arc<Notify>,
    connected: Arc<AtomicBool>,
    recorder: Arc<Recorder>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        self.recorder
            .event(format!("desktop ice gathering={state}"));
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        self.recorder.event(format!("desktop ice={state}"));
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.recorder.event(format!("desktop connection={state}"));
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
    }
}
/// Where the desktop peer listens. Production binds loopback only; the
/// interface variant exists for a short, video-free connectivity self-test.
#[derive(Clone, Copy)]
pub enum Bind {
    Loopback,
    Interfaces,
}
pub struct Preview {
    track: Arc<TrackLocalStaticRTP>,
    payload_type: u8,
    pub connected: Arc<AtomicBool>,
    peer: Arc<dyn PeerConnection>,
    runtime: tokio::runtime::Handle,
    recorder: Arc<Recorder>,
    first_write: AtomicBool,
}
impl Preview {
    pub async fn answer(offer: &str) -> Result<(Self, String), String> {
        Self::answer_with(offer, Bind::Loopback, Recorder::new()).await
    }
    pub async fn answer_with(
        offer: &str,
        bind: Bind,
        recorder: Arc<Recorder>,
    ) -> Result<(Self, String), String> {
        let result = Self::negotiate(offer, bind, recorder.clone()).await;
        match &result {
            Ok((_, sdp)) => {
                let (candidates, _) = crate::video_bridge::candidate_summary(sdp);
                recorder.fact("answer.candidates", candidates);
                recorder.event("desktop answer ready");
            }
            Err(error) => {
                recorder.fact(crate::video_bridge::ERROR, error);
                recorder.event(format!("desktop setup failed: {error}"));
            }
        }
        result
    }
    async fn negotiate(
        offer: &str,
        bind: Bind,
        recorder: Arc<Recorder>,
    ) -> Result<(Self, String), String> {
        if offer.len() > 64 * 1024 {
            return Err("Invalid screen viewer offer".into());
        }
        let mut media = MediaEngine::default();
        media
            .register_codec(
                rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
                    rtp_codec: rtc::rtp_transceiver::rtp_sender::RTCRtpCodec {
                        mime_type: "video/H264".into(),
                        clock_rate: 90_000,
                        sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                        ..Default::default()
                    },
                    payload_type: thiscord_shared::voice::MediaKind::ScreenVideo.payload_type(),
                },
                rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Video,
            )
            .map_err(|_| "Cannot configure video codec")?;
        let registry = register_default_interceptors(Registry::new(), &mut media)
            .map_err(|_| "Cannot configure video transport")?;
        let gathered = Arc::new(Notify::new());
        let connected = Arc::new(AtomicBool::new(false));
        let addresses = match bind {
            Bind::Loopback => vec!["127.0.0.1:0".to_owned()],
            // Wildcards expand to one socket per non-loopback interface address.
            Bind::Interfaces => vec!["0.0.0.0:0".to_owned(), "[::]:0".to_owned()],
        };
        let peer: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_runtime(Arc::new(
                    crate::audio::media_runtime::MediaRuntime::default(),
                ))
                .with_media_engine(media)
                .with_interceptor_registry(registry)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    connected: connected.clone(),
                    recorder: recorder.clone(),
                }))
                .with_udp_addrs(addresses)
                .build()
                .await
                .map_err(|_| "Cannot initialize local video transport")?,
        );
        let track = super::track(thiscord_shared::voice::MediaKind::ScreenVideo.publisher_ssrc());
        let preview = Self {
            track,
            payload_type: 0,
            connected,
            peer,
            runtime: tokio::runtime::Handle::current(),
            recorder,
            first_write: AtomicBool::new(true),
        };
        let offer = serde_json::from_value(serde_json::json!({"type":"offer","sdp":offer}))
            .map_err(|_| "Invalid screen viewer offer")?;
        preview
            .peer
            .set_remote_description(offer)
            .await
            .map_err(|_| "WebView does not support the screen video codec")?;
        preview
            .peer
            .add_track(preview.track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| "Cannot attach screen video")?;
        let answer = preview
            .peer
            .create_answer(None)
            .await
            .map_err(|_| "Cannot create screen viewer answer")?;
        preview
            .peer
            .set_local_description(answer)
            .await
            .map_err(|_| "Cannot initialize screen viewer")?;
        tokio::time::timeout(Duration::from_secs(5), gathered.notified())
            .await
            .map_err(|_| "Local video negotiation timed out")?;
        let answer = preview
            .peer
            .local_description()
            .await
            .ok_or("Missing screen viewer answer")?;
        let json = serde_json::to_value(answer).map_err(|_| "Invalid screen viewer answer")?;
        let sdp = json["sdp"]
            .as_str()
            .ok_or("Missing screen viewer SDP")?
            .to_owned();
        let payload_type = sdp
            .lines()
            .find_map(|line| {
                let (pt, codec) = line.strip_prefix("a=rtpmap:")?.split_once(' ')?;
                codec
                    .trim()
                    .eq_ignore_ascii_case("H264/90000")
                    .then(|| pt.parse::<u8>().ok())
                    .flatten()
            })
            .ok_or("WebView did not negotiate H.264 video")?;
        let mut preview = preview;
        preview.payload_type = payload_type;
        preview.recorder.fact("answer.payload_type", payload_type);
        Ok((preview, sdp))
    }
    pub async fn write(&self, mut packet: rtc::rtp::Packet) -> Result<(), String> {
        // Browser-assigned dynamic payload types differ from the SFU contract.
        packet.header.payload_type = self.payload_type;
        packet.header.ssrc = thiscord_shared::voice::MediaKind::ScreenVideo.publisher_ssrc();
        let result = self
            .track
            .write_rtp(packet)
            .await
            .map_err(|_| "Local video transport closed".to_owned());
        if self.first_write.swap(false, Ordering::AcqRel) {
            self.recorder.event(match &result {
                Ok(()) => "first video packet forwarded to the WebView",
                Err(_) => "first video packet write failed",
            });
        }
        result
    }
    /// Records the current ICE/DTLS state without exposing addresses.
    pub async fn sample(&self, recorder: &Recorder) {
        let report = self
            .peer
            .get_stats(
                std::time::Instant::now(),
                rtc::statistics::StatsSelector::None,
            )
            .await;
        recorder.apply_stats(&super::bridge::stats_facts(&report));
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        let peer = self.peer.clone();
        self.connected.store(false, Ordering::Release);
        self.recorder.event("desktop peer closed");
        self.runtime.spawn(async move {
            let _ = peer.close().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::{
        rtp,
        rtp_transceiver::{
            RTCRtpTransceiverDirection, RTCRtpTransceiverInit, rtp_sender::RtpCodecKind,
        },
    };
    use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
    struct Receiver {
        gathered: Arc<Notify>,
        packets: tokio::sync::mpsc::Sender<rtp::Packet>,
    }
    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for Receiver {
        async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
            if state == RTCIceGatheringState::Complete {
                self.gathered.notify_one();
            }
        }
        async fn on_track(&self, track: Arc<dyn TrackRemote>) {
            let packets = self.packets.clone();
            tokio::spawn(async move {
                while let Some(event) = track.poll().await {
                    if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                        let _ = packets.try_send(packet);
                    }
                }
            });
        }
    }
    #[tokio::test]
    async fn h264_crosses_local_video_only_peer_without_transcoding() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut media = MediaEngine::default();
            media.register_codec(rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
                rtp_codec: rtc::rtp_transceiver::rtp_sender::RTCRtpCodec {
                    mime_type:"video/H264".into(),clock_rate:90_000,
                    sdp_fmtp_line:"level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".into(),
                    ..Default::default()
                }, payload_type:102,
            },RtpCodecKind::Video).unwrap();
            let gathered = Arc::new(Notify::new());
            let (packets, mut received) = tokio::sync::mpsc::channel(128);
            let receiver = PeerConnectionBuilder::new().with_runtime(Arc::new(crate::audio::media_runtime::MediaRuntime::default()))
                .with_media_engine(media)
                .with_handler(Arc::new(Receiver {
                    gathered: gathered.clone(),
                    packets,
                }))
                .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
                .build()
                .await
                .unwrap();
            receiver
                .add_transceiver_from_kind(
                    RtpCodecKind::Video,
                    Some(RTCRtpTransceiverInit {
                        direction: RTCRtpTransceiverDirection::Recvonly,
                        ..Default::default()
                    }),
                )
                .await
                .unwrap();
            let offer = receiver.create_offer(None).await.unwrap();
            receiver.set_local_description(offer).await.unwrap();
            gathered.notified().await;
            let offer = serde_json::to_value(receiver.local_description().await.unwrap()).unwrap();
            let recorder = Recorder::new();
            let (preview, answer) = Preview::answer_with(
                offer["sdp"].as_str().unwrap(),
                Bind::Loopback,
                recorder.clone(),
            )
            .await
            .unwrap();
            assert!(!answer.contains("m=audio"));
            assert!(
                answer
                    .lines()
                    .filter(|l| l.starts_with("a=candidate:"))
                    .all(|l| l.contains("127.0.0.1"))
            );
            receiver
                .set_remote_description(
                    serde_json::from_value(serde_json::json!({"type":"answer","sdp":answer}))
                        .unwrap(),
                )
                .await
                .unwrap();
            while !preview.connected.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            use openh264::{
                encoder::{Encoder, EncoderConfig, Profile, UsageType},
                formats::{RgbaSliceU8, YUVBuffer},
            };
            let rgba = vec![128; 320 * 180 * 4];
            let mut yuv = YUVBuffer::new(320, 180);
            yuv.read_rgba8(RgbaSliceU8::new(&rgba, (320, 180)));
            let mut encoder = Encoder::with_api_config(
                openh264::OpenH264API::from_source(),
                EncoderConfig::new()
                    .profile(Profile::Baseline)
                    .usage_type(UsageType::ScreenContentRealTime),
            )
            .unwrap();
            let data = encoder.encode(&yuv).unwrap().to_vec();
            for packet in super::super::packetize(data.clone(), &mut 0, 9000).unwrap() {
                preview.write(packet).await.unwrap();
            }
            let mut assembler = super::super::Assembler::default();
            loop {
                let packet = received.recv().await.unwrap();
                assert_eq!(packet.header.payload_type,102);
                if let Some(frame) = assembler.push(&packet) {
                    assert_eq!(
                        frame, data,
                        "local presentation must not decode/re-encode video"
                    );
                    break;
                }
            }
            // Diagnostics describe the hop without exposing its addresses.
            preview.sample(&recorder).await;
            let section = recorder.snapshot();
            let facts = &section.facts;
            assert!(facts[crate::video_bridge::RESPONSES_SENT].parse::<u64>().unwrap() > 0);
            assert!(facts["candidates.local"].contains("loopback-v4"), "{facts:?}");
            assert!(facts["pair.0"].contains("udp host loopback-v4→"), "{facts:?}");
            assert_eq!(facts[crate::video_bridge::DTLS_STATE], "connected");
            assert!(
                section
                    .events
                    .iter()
                    .any(|(_, e)| e == "first video packet forwarded to the WebView")
            );
            let wire = serde_json::to_string(&section).unwrap();
            assert!(!wire.contains("127.0.0.1"), "{wire}");
            // Last-reference release is allowed on a capture/relay worker.
            std::thread::spawn(move || drop(preview)).join().unwrap();
            receiver.close().await.unwrap();
        })
        .await
        .expect("local video bridge stalled");
    }
}
