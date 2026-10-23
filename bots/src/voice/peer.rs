use super::*;
use bytes::Bytes;
use rtc::{
    media_stream::MediaStreamTrack,
    rtp,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
    },
};
use thiscord_shared::voice::{IceServer, MediaKind};
use tokio::sync::Notify;
use webrtc::{
    media_stream::track_local::static_rtp::TrackLocalStaticRTP, peer_connection::*,
    rtp_transceiver::RTCRtpTransceiverDirection,
};

struct Handler {
    gathered: Arc<Notify>,
    state: watch::Sender<RTCPeerConnectionState>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.state.send_replace(state);
    }
    // Deliberately no on_track callback or decoder. SDP disables all receivers.
}

pub(super) struct Peer {
    pub(super) pc: Arc<dyn PeerConnection>,
    pub(super) track: Arc<TrackLocalStaticRTP>,
    pub(super) video: Arc<TrackLocalStaticRTP>,
    pub(super) stream_audio: Arc<TrackLocalStaticRTP>,
    pub(super) state: watch::Receiver<RTCPeerConnectionState>,
    gathered: Arc<Notify>,
    closed: bool,
}

pub(super) fn usable(state: RTCPeerConnectionState) -> bool {
    !matches!(
        state,
        RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Closed
    )
}

impl Peer {
    pub(super) async fn new(servers: &[IceServer]) -> Result<Self, Error> {
        let gathered = Arc::new(Notify::new());
        let (state_tx, state) = watch::channel(RTCPeerConnectionState::New);
        let mut engine = MediaEngine::default();
        engine
            .register_codec(
                rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
                    rtp_codec: codec(MediaKind::ScreenVideo),
                    payload_type: MediaKind::ScreenVideo.payload_type(),
                },
                RtpCodecKind::Video,
            )
            .map_err(|_| Error::message("Video codec setup failed"))?;
        engine
            .register_default_codecs()
            .map_err(|_| Error::message("Voice codec setup failed"))?;
        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(
                servers
                    .iter()
                    .map(|s| RTCIceServer {
                        urls: s.urls.clone(),
                        username: s.username.clone(),
                        credential: s.credential.clone(),
                    })
                    .collect(),
            )
            .build();
        let pc = PeerConnectionBuilder::new()
            .with_configuration(config)
            .with_media_engine(engine)
            .with_handler(Arc::new(Handler {
                gathered: gathered.clone(),
                state: state_tx,
            }))
            // Wildcard expansion in webrtc 0.21 deliberately excludes loopback.
            // Keep an explicit loopback candidate for a locally hosted backend.
            .with_udp_addrs(vec!["0.0.0.0:0".to_owned(), "127.0.0.1:0".to_owned()])
            .build()
            .await
            .map_err(|_| Error::message("Voice peer setup failed"))?;
        Ok(Self {
            pc: Arc::new(pc),
            track: track(),
            video: media_track(MediaKind::ScreenVideo),
            stream_audio: media_track(MediaKind::SystemAudio),
            gathered,
            state,
            closed: false,
        })
    }

    #[cfg(test)]
    pub(super) async fn answer(&self, sdp: &str) -> Result<String, Error> {
        self.answer_with_screen(sdp, false).await
    }

    pub(super) async fn answer_with_screen(
        &self,
        sdp: &str,
        screen: bool,
    ) -> Result<String, Error> {
        self.pc
            .set_remote_description(
                serde_json::from_str(sdp).map_err(|_| Error::message("Invalid voice offer"))?,
            )
            .await
            .map_err(|_| Error::message("Voice offer rejected"))?;
        let sender = self
            .pc
            .add_track(self.track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|_| Error::message("Voice track setup failed"))?;
        let mut senders = vec![sender.id()];
        if screen {
            for track in [&self.video, &self.stream_audio] {
                senders.push(
                    self.pc
                        .add_track(track.clone() as Arc<dyn TrackLocal>)
                        .await
                        .map_err(|_| Error::message("Screen-share track setup failed"))?
                        .id(),
                );
            }
        }
        for transceiver in self.pc.get_transceivers().await {
            let ours = transceiver
                .sender()
                .await
                .map_err(|_| Error::message("Voice direction setup failed"))?
                .is_some_and(|candidate| senders.contains(&candidate.id()));
            transceiver
                .set_direction(if ours {
                    RTCRtpTransceiverDirection::Sendonly
                } else {
                    RTCRtpTransceiverDirection::Inactive
                })
                .await
                .map_err(|_| Error::message("Voice direction setup failed"))?;
        }
        let answer = self
            .pc
            .create_answer(None)
            .await
            .map_err(|_| Error::message("Voice answer failed"))?;
        self.pc
            .set_local_description(answer)
            .await
            .map_err(|_| Error::message("Voice answer failed"))?;
        // The SFU allows 15 seconds for negotiation. A STUN/TURN request from
        // an unreachable interface (notably loopback) can take longer than
        // that, even after usable host or relay candidates have been gathered.
        // Send the candidates available within our budget instead of holding
        // the entire answer hostage to the slowest ICE server/interface.
        let _ = timeout(Duration::from_secs(8), self.gathered.notified()).await;
        let answer = self
            .pc
            .local_description()
            .await
            .ok_or_else(|| Error::message("Missing voice answer"))?;
        if !answer
            .sdp
            .lines()
            .any(|line| line.starts_with("a=candidate:"))
        {
            return Err(Error::message(
                "No voice network candidates; check the bot host network and STUN/TURN configuration",
            ));
        }
        serde_json::to_string(&answer).map_err(|_| Error::message("Voice answer encoding failed"))
    }

    pub(super) async fn connected(&self) -> Result<(), Error> {
        let mut state = self.state.clone();
        state
            .wait_for(|state| *state == RTCPeerConnectionState::Connected || !usable(*state))
            .await
            .map_err(|_| Error::message("Voice media disconnected"))?;
        if *state.borrow() != RTCPeerConnectionState::Connected {
            return Err(Error::message("Voice media connection failed"));
        }
        Ok(())
    }

    pub(super) async fn close(&mut self) {
        let _ = timeout(IO_TIMEOUT, self.pc.close()).await;
        self.closed = true;
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if !self.closed
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let pc = self.pc.clone();
            runtime.spawn(async move {
                let _ = timeout(IO_TIMEOUT, pc.close()).await;
            });
        }
    }
}

pub(super) fn track() -> Arc<TrackLocalStaticRTP> {
    media_track(MediaKind::Microphone)
}

pub(super) fn codec(kind: MediaKind) -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: if kind.is_video() {
            "video/H264"
        } else {
            "audio/opus"
        }
        .into(),
        clock_rate: kind.clock_rate(),
        channels: if kind.is_video() { 0 } else { 2 },
        sdp_fmtp_line: if kind.is_video() {
            thiscord_shared::screen::H264_FMTP
        } else {
            "minptime=10;useinbandfec=1"
        }
        .into(),
        ..Default::default()
    }
}

pub(super) fn media_track(kind: MediaKind) -> Arc<TrackLocalStaticRTP> {
    Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("bot-{kind:?}"),
        format!("bot-{kind:?}"),
        "bot-media".into(),
        if kind.is_video() {
            RtpCodecKind::Video
        } else {
            RtpCodecKind::Audio
        },
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(kind.publisher_ssrc()),
                ..Default::default()
            },
            codec: codec(kind),
            ..Default::default()
        }],
    )))
}

pub(super) struct RtpClock {
    sequence: u16,
    timestamp: u32,
    start: Instant,
    next_sample: u64,
}
impl RtpClock {
    pub(super) fn new() -> Self {
        Self {
            sequence: rand::random(),
            timestamp: rand::random(),
            start: Instant::now(),
            next_sample: 0,
        }
    }
    pub(super) fn packet(&mut self, payload: Bytes, marker: bool) -> rtp::Packet {
        self.sequence = self.sequence.wrapping_add(1);
        // Preserve RTP time across silence and replacement, without resetting
        // sequence numbers under the same SRTP SSRC/key.
        let sample = ((self.start.elapsed().as_millis() as u64 / 20) * 960).max(self.next_sample);
        self.next_sample = sample + 960;
        let timestamp = self.timestamp.wrapping_add(sample as u32);
        rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type: MediaKind::Microphone.payload_type(),
                ssrc: MediaKind::Microphone.publisher_ssrc(),
                sequence_number: self.sequence,
                timestamp,
                marker,
                ..Default::default()
            },
            payload,
        }
    }
}
