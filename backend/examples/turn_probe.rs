//! Manual live-infrastructure probe; no account, microphone or database required.
//! Run from WSL: cargo run -p thiscord-backend --example turn_probe --locked
#[path = "../src/voice/ice.rs"]
mod ice;

use rtc::{media_stream::MediaStreamTrack, rtp, rtp_transceiver::rtp_sender::*};
use std::{error::Error, sync::Arc, time::Duration};
use thiscord_shared::AccountId;
use tokio::sync::{Notify, mpsc};
use webrtc::{
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Handler {
    gathered: Arc<Notify>,
    packets: mpsc::Sender<rtp::Packet>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
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

struct Peer {
    pc: Arc<dyn PeerConnection>,
    gathered: Arc<Notify>,
    track: Arc<TrackLocalStaticRTP>,
    packets: mpsc::Receiver<rtp::Packet>,
}
impl Peer {
    async fn new(bind: &str, relay: bool) -> Result<Self> {
        let mut configuration = if relay {
            let servers = ice::servers(AccountId::from_uuid(uuid::Uuid::new_v4()))?;
            ice::configuration(&servers)
        } else {
            RTCConfiguration::default()
        };
        if relay {
            configuration = RTCConfigurationBuilder::new()
                .with_ice_servers(configuration.ice_servers().to_vec())
                .with_ice_transport_policy(RTCIceTransportPolicy::Relay)
                .build();
        }
        let gathered = Arc::new(Notify::new());
        let (tx, packets) = mpsc::channel(64);
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(configuration)
                .with_media_engine(media)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    packets: tx,
                }))
                .with_udp_addrs(vec![bind.to_owned()])
                .build()
                .await?,
        );
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            "probe".into(),
            "probe".into(),
            "probe".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(900),
                    ..Default::default()
                },
                codec: RTCRtpCodec {
                    mime_type: "audio/opus".into(),
                    clock_rate: 48_000,
                    channels: 2,
                    ..Default::default()
                },
                ..Default::default()
            }],
        )));
        pc.add_track(track.clone() as Arc<dyn TrackLocal>).await?;
        Ok(Self {
            pc,
            gathered,
            track,
            packets,
        })
    }

    async fn selected_types(&self) -> Result<(String, String)> {
        let senders = self.pc.get_senders().await;
        let dtls = senders
            .first()
            .ok_or("Missing sender")?
            .transport()
            .await?
            .ok_or("Missing transport")?;
        let pair = dtls
            .ice_transport()
            .get_selected_candidate_pair()
            .await?
            .ok_or("No selected candidate pair")?;
        Ok((pair.local().typ.to_string(), pair.remote().typ.to_string()))
    }
}

async fn direction(source: &Peer, receiver: &mut Peer) -> Result<()> {
    // Keep sending during ICE/DTLS establishment, then require real decrypted RTP.
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut received = 0;
        let mut sequence = 0_u16;
        loop {
            sequence = sequence.wrapping_add(1);
            source
                .track
                .write_rtp(rtp::Packet {
                    header: rtp::header::Header {
                        version: 2,
                        payload_type: 111,
                        ssrc: 900,
                        sequence_number: sequence,
                        timestamp: u32::from(sequence) * 960,
                        ..Default::default()
                    },
                    // Synthetic Opus silence, never microphone data.
                    payload: vec![0xf8, 0xff, 0xfe].into(),
                })
                .await?;
            if let Ok(Some(packet)) =
                tokio::time::timeout(Duration::from_millis(20), receiver.packets.recv()).await
            {
                if packet.payload.as_ref() != [0xf8, 0xff, 0xfe] {
                    return Err("Unexpected test payload".into());
                }
                received += 1;
                if received == 10 {
                    return Ok::<_, Box<dyn Error>>(());
                }
            }
        }
    })
    .await?
}

async fn probe(bind: &str, both_relay: bool) -> Result<()> {
    let label = if both_relay {
        "relay-to-relay"
    } else {
        "relay-to-WSL-host"
    };
    println!("Testing {label} (direct fallback disabled on sender)...");
    let mut a = Peer::new(bind, true).await?;
    let mut b = match Peer::new(bind, both_relay).await {
        Ok(peer) => peer,
        Err(error) => {
            let _ = a.pc.close().await;
            return Err(error);
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(55), async {
        a.pc.set_local_description(a.pc.create_offer(None).await?).await?;
        a.gathered.notified().await;
        b.pc.set_remote_description(a.pc.local_description().await.ok_or("Missing offer")?).await?;
        b.pc.set_local_description(b.pc.create_answer(None).await?).await?;
        b.gathered.notified().await;
        a.pc.set_remote_description(b.pc.local_description().await.ok_or("Missing answer")?).await?;
        direction(&a, &mut b).await?;
        direction(&b, &mut a).await?;
        let (local, remote) = a.selected_types().await?;
        if local != "relay" || (both_relay && remote != "relay") {
            return Err("Probe did not select the required relay candidates".into());
        }
        println!("PASS {label}: selected {local} -> {remote}; received 10 encrypted RTP packets each direction");
        Ok::<_, Box<dyn Error>>(())
    }).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), a.pc.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), b.pc.close()).await;
    result?
}

#[tokio::main]
async fn main() {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    if std::env::var("THISCORD_TURN_URL").is_err() || std::env::var("THISCORD_TURN_SECRET").is_err()
    {
        eprintln!("Configure THISCORD_TURN_URL and THISCORD_TURN_SECRET in backend/.env first.");
        std::process::exit(1);
    }
    let result: Result<()> = async {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
        socket.connect("192.0.2.1:9")?;
        let default_bind = format!("{}:0", socket.local_addr()?.ip());
        let bind = std::env::var("THISCORD_VOICE_BIND").unwrap_or(default_bind);
        probe(&bind, true).await?;
        probe(&bind, false).await
    }
    .await;
    if result.is_err() {
        // Dependency errors can contain signaling details; never print them.
        eprintln!("FAIL: relay negotiation or packet delivery failed in the stage above.");
        std::process::exit(1);
    }
}
