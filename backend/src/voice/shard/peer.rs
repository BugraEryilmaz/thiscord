//! One connection's sans-I/O core and the per-connection state that the webrtc
//! mode keeps in `Handler`, its track reader tasks and its egress writers.
use super::{forward::Egress, *};
use rtc::{
    ice::{candidate::CandidateConfig, mdns::MulticastDnsMode},
    peer_connection::{
        RTCPeerConnectionBuilder,
        configuration::{RTCConfigurationBuilder, setting_engine::SettingEngineBuilder},
        event::{RTCPeerConnectionEvent, RTCTrackEvent, RTCTrackEventInit},
        message::RTCMessage,
        state::RTCPeerConnectionState,
        transport::{CandidateHostConfig, RTCIceCandidate, RTCIceCandidateInit},
    },
    rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication,
    rtp_transceiver::{RTCRtpReceiverId, RTCRtpSenderId},
};

/// A classified inbound track: what `Handler::on_track` decided, plus the
/// per-second limits its reader task enforced.
struct Inbound {
    track_id: String,
    kind: MediaKind,
    ssrc: u32,
    since: Instant,
    count: usize,
    bytes: usize,
}

pub(super) struct Connection {
    pub id: PeerId,
    pub core: RTCPeerConnection,
    pub context: PeerContext,
    /// Relay track senders by `MediaKind` track index.
    pub senders: Vec<RTCRtpSenderId>,
    /// Local mid -> track index, from the offer (browser publishers).
    mids: HashMap<String, usize>,
    inbound: Vec<Inbound>,
    track_seen: [bool; 5],
    pub browser: bool,
    /// Own-slot browser screen and camera receivers, for PLI.
    browser_video: [Option<(u32, RTCRtpReceiverId)>; 2],
    screen_rate: layers::IngressBudget,
    /// Activated after negotiation, as the webrtc mode spawns relay and writers.
    pub forwarding: bool,
    /// The webrtc relay task exits for good on a slot or source mismatch.
    pub relaying: bool,
    pub egress: [Egress; 4],
    pub performance_sample: u64,
    pub deadline: Option<Instant>,
    pub timed_out_at: Option<Instant>,
    pub input: bool,
    pub output: bool,
}

impl Connection {
    pub fn new(id: PeerId, context: PeerContext, now: Instant) -> Result<Self, ()> {
        let (engine, registry) = media_setup();
        let core = RTCPeerConnectionBuilder::new()
            // No STUN/TURN gathering here: the shard advertises its host
            // candidate; clients reach it directly or through their own relay.
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_media_engine(engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(
                SettingEngineBuilder::new()
                    .with_multicast_dns_mode(MulticastDnsMode::Disabled)
                    .build(),
            )
            .build(now)
            .map_err(|error| {
                tracing::warn!(request_id = %context.request_id, %error, "voice peer build failed");
            })?;
        Ok(Self {
            id,
            core,
            context,
            senders: Vec::with_capacity(MediaKind::TRACK_COUNT),
            mids: HashMap::new(),
            inbound: Vec::new(),
            track_seen: [false; 5],
            browser: false,
            browser_video: [None; 2],
            screen_rate: Default::default(),
            forwarding: false,
            relaying: true,
            egress: Default::default(),
            performance_sample: 0,
            deadline: None,
            timed_out_at: None,
            input: false,
            output: false,
        })
    }

    pub fn offer(
        &mut self,
        now: Instant,
        local: std::net::SocketAddr,
    ) -> Result<RTCSessionDescription, ()> {
        if !self.senders.is_empty() {
            return Err(());
        }
        for slot in 0..MediaKind::TRACK_COUNT {
            let sender = self
                .core
                .add_track(local_media_track(slot))
                .map_err(|_| ())?;
            self.senders.push(sender);
        }
        let offer = self.core.create_offer(None).map_err(|_| ())?;
        self.core
            .set_local_description(now, offer)
            .map_err(|_| ())?;
        let candidate = CandidateHostConfig {
            base_config: CandidateConfig {
                network: "udp".to_owned(),
                address: local.ip().to_string(),
                port: local.port(),
                component: 1,
                ..Default::default()
            },
            ..Default::default()
        }
        .new_candidate_host()
        .map_err(|_| ())?;
        let candidate = RTCIceCandidate::from(&candidate)
            .to_json()
            .map_err(|_| ())?;
        self.core.add_local_candidate(candidate).map_err(|_| ())?;
        // End of candidates: gathering completes, as the webrtc driver signals.
        self.core
            .add_local_candidate(RTCIceCandidateInit::default())
            .map_err(|_| ())?;
        let local = self.core.local_description().ok_or(())?;
        self.mids = browser::mids(&local.sdp);
        Ok(local)
    }

    pub fn events(&mut self) {
        while let Some(event) = self.core.poll_event() {
            match event {
                RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) => {
                    tracing::info!(request_id = %self.context.request_id, state = ?state, "voice peer state changed");
                    if matches!(
                        state,
                        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                    ) {
                        self.context.active.store(false, Ordering::Release);
                    }
                }
                RTCPeerConnectionEvent::OnTrack(RTCTrackEvent::OnOpen(init)) => {
                    self.open_track(init);
                }
                _ => {}
            }
        }
    }

    /// `Handler::on_track`: classify once per track; reject what the webrtc
    /// mode rejects (deactivating the connection) and ignore what it ignores.
    fn open_track(&mut self, init: RTCTrackEventInit) {
        // Simulcast repeats OnOpen per RID; the webrtc layer announces once.
        if self.inbound.iter().any(|t| t.track_id == init.track_id) {
            return;
        }
        let browser = self.browser;
        let slot = self.context.slot;
        let mid = self
            .core
            .rtp_transceiver(init.receiver_id.into())
            .and_then(|transceiver| transceiver.mid().clone());
        let Some(receiver) = self.core.rtp_receiver(init.receiver_id) else {
            return;
        };
        // The webrtc layer gives the track only the coding of the SSRC that
        // opened it; without one the track has no SSRC at all.
        let Some(codec) = receiver
            .track()
            .codings()
            .iter()
            .find(|coding| coding.rtp_coding_parameters.ssrc == Some(init.ssrc))
            .map(|coding| coding.codec.clone())
        else {
            self.context.active.store(false, Ordering::Release);
            return;
        };
        let ssrc = init.ssrc;
        let browser_kind = if browser {
            // Only the member's own m-lines publish; others stay unread.
            let kind = mid
                .and_then(|mid| self.mids.get(&mid).copied())
                .and_then(MediaKind::from_track)
                .and_then(|(kind, owner)| (owner == slot).then_some(kind));
            if kind.is_none() {
                tracing::info!(request_id = %self.context.request_id, "ignored browser track outside its own media lines");
                return;
            }
            kind
        } else {
            None
        };
        let low_layer = !browser && ssrc == thiscord_shared::screen::LOW_LAYER_SSRC;
        let Some(kind) = (if browser {
            browser_kind
        } else if low_layer {
            Some(MediaKind::ScreenVideo)
        } else {
            MediaKind::from_publisher_ssrc(ssrc)
        }) else {
            self.context.active.store(false, Ordering::Release);
            return;
        };
        let mime = if kind.is_video() {
            "video/H264"
        } else {
            "audio/opus"
        };
        let seen = &mut self.track_seen[if low_layer { 4 } else { kind as usize }];
        if !(codec.mime_type.eq_ignore_ascii_case(mime)
            || (kind == MediaKind::Microphone
                && !browser
                && codec
                    .mime_type
                    .eq_ignore_ascii_case(thiscord_shared::rtp_red::MIME)))
            || codec.clock_rate != kind.clock_rate()
            || std::mem::replace(seen, true)
        {
            self.context.active.store(false, Ordering::Release);
            return;
        }
        if browser && kind.is_video() {
            self.browser_video[kind.view_index(0) / ROOM_CAPACITY] = Some((ssrc, init.receiver_id));
        }
        self.inbound.push(Inbound {
            track_id: init.track_id,
            kind,
            ssrc,
            since: Instant::now(),
            count: 0,
            bytes: 0,
        });
    }

    /// Drains decrypted media, applying the track reader's checks. RTCP
    /// surfaced to the application is ignored, as the webrtc mode never reads it.
    pub fn media(&mut self, out: &mut Vec<(MediaKind, rtp::Packet, Instant)>) {
        while let Some(message) = self.core.poll_media_read() {
            let RTCMessage::RtpPacket(track_id, packet) = message.message else {
                continue;
            };
            if !self.context.active.load(Ordering::Acquire) {
                continue;
            }
            let Some(track) = self.inbound.iter_mut().find(|t| t.track_id == track_id) else {
                continue;
            };
            if packet.header.ssrc != track.ssrc {
                continue;
            }
            let kind = track.kind;
            if kind == MediaKind::ScreenVideo && !self.screen_rate.admit(packet.payload.len()) {
                self.context.active.store(false, Ordering::Release);
                continue;
            }
            if track.since.elapsed() > Duration::from_secs(1) {
                track.since = Instant::now();
                track.count = 0;
                track.bytes = 0;
            }
            track.count += 1;
            track.bytes += packet.payload.len();
            if track.count
                > (if kind.is_video() {
                    thiscord_shared::screen::MAX_PACKETS_PER_SECOND
                } else {
                    100
                })
                || packet.payload.len() > 1500
                || track.bytes
                    > if kind.is_video() {
                        thiscord_shared::screen::MAX_BYTES_PER_SECOND
                    } else {
                        150_000
                    }
            {
                self.context.active.store(false, Ordering::Release);
                continue;
            }
            if kind.is_video() {
                self.context.metrics[0].fetch_add(1, Ordering::Relaxed);
            }
            // Before negotiation completes nothing relays (the webrtc relay
            // task does not exist yet either).
            if self.forwarding && self.relaying {
                out.push((kind, packet, message.now));
            }
        }
    }

    /// Browsers answer RTCP PLI on their media SSRC with an IDR.
    pub fn keyframe(&mut self, now: Instant, kind: MediaKind) {
        if !kind.is_video() {
            return;
        }
        let Some((ssrc, receiver)) = self.browser_video[kind.view_index(0) / ROOM_CAPACITY] else {
            return;
        };
        let sender_ssrc = media_ssrc(kind.track_index(self.context.slot));
        if let Some(mut receiver) = self.core.rtp_receiver(receiver) {
            let _ = receiver.write_rtcp(
                now,
                vec![Box::new(PictureLossIndication {
                    sender_ssrc,
                    media_ssrc: ssrc,
                })],
            );
        }
    }
}
