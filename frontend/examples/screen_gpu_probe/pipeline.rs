use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit, rtp_sender::*};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_frontend::{
    audio::connection::Connection,
    screen::{self, adaptation::Demand, metrics::Metrics, receive::Inbox},
};
use thiscord_shared::screen::{Quality, SourceId};
use tokio::sync::Notify;
use webrtc::{
    media_stream::{
        track_local::TrackLocal,
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

struct Events {
    gathered: Arc<Notify>,
    connected: Arc<AtomicBool>,
    incoming: Mutex<Option<screen::receive::Sender>>,
    keyframe: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connected.store(
            state == RTCPeerConnectionState::Connected,
            Ordering::Release,
        );
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let Some(incoming) = self.incoming.lock().unwrap().take() else {
            return;
        };
        let keyframe = self.keyframe.clone();
        tokio::spawn(async move {
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(mut packet) = event {
                    // Equivalent to the SFU's stream generation tag. No server
                    // permissions/database are exercised by this loopback probe.
                    packet.header.csrc = vec![1];
                    incoming.push(packet, Instant::now());
                    if incoming.keyframe_request(Instant::now()).is_some() {
                        keyframe.store(true, Ordering::Release);
                    }
                }
            }
        });
    }
}
fn engine() -> Result<MediaEngine, Box<dyn std::error::Error>> {
    let mut media = MediaEngine::default();
    media.register_codec(
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: "video/H264".into(),
                clock_rate: 90_000,
                sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                ..Default::default()
            },
            payload_type: thiscord_shared::voice::MediaKind::ScreenVideo.payload_type(),
        },
        RtpCodecKind::Video,
    )?;
    Ok(media)
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let seconds = std::env::args()
        .find_map(|arg| {
            arg.strip_prefix("--seconds=")
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(30)
        .clamp(5, 300);
    let quality = Quality {
        height: 2160,
        fps: 60,
    };
    let preview = if std::env::args().any(|arg| arg == "--browser") {
        Some(tokio::time::timeout(Duration::from_secs(120), super::browser::connect()).await??)
    } else {
        None
    };
    if let Some(preview) = &preview {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !preview.connected.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
    }
    let metrics = Metrics::default();
    let force_keyframe = Arc::new(AtomicBool::new(true));
    let (incoming, inbox) = Inbox::channel();
    let receive_metrics = inbox.metrics();
    let receiver_gathered = Arc::new(Notify::new());
    let publisher_gathered = Arc::new(Notify::new());
    let connected = Arc::new(AtomicBool::new(false));
    let receiver = Arc::new(
        PeerConnectionBuilder::new()
            .with_runtime(Arc::new(
                thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
            ))
            .with_media_engine(engine()?)
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .with_handler(Arc::new(Events {
                gathered: receiver_gathered.clone(),
                connected: Arc::default(),
                incoming: Mutex::new(Some(incoming)),
                keyframe: force_keyframe.clone(),
            }))
            .build()
            .await?,
    );
    let mut media = engine()?;
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let publisher = Arc::new(
        PeerConnectionBuilder::new()
            .with_runtime(Arc::new(
                thiscord_frontend::audio::media_runtime::MediaRuntime::default(),
            ))
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
            .with_handler(Arc::new(Events {
                gathered: publisher_gathered.clone(),
                connected: connected.clone(),
                incoming: Mutex::new(None),
                keyframe: force_keyframe.clone(),
            }))
            .build()
            .await?,
    );
    receiver
        .add_transceiver_from_kind(
            RtpCodecKind::Video,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                ..Default::default()
            }),
        )
        .await?;
    let offer = receiver.create_offer(None).await?;
    receiver.set_local_description(offer).await?;
    tokio::time::timeout(Duration::from_secs(10), receiver_gathered.notified()).await?;
    publisher
        .set_remote_description(receiver.local_description().await.ok_or("No offer")?)
        .await?;
    let track = screen::track(thiscord_shared::voice::MediaKind::ScreenVideo.publisher_ssrc());
    publisher
        .add_track(track.clone() as Arc<dyn TrackLocal>)
        .await?;
    let answer = publisher.create_answer(None).await?;
    publisher.set_local_description(answer).await?;
    tokio::time::timeout(Duration::from_secs(10), publisher_gathered.notified()).await?;
    receiver
        .set_remote_description(publisher.local_description().await.ok_or("No answer")?)
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !connected.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;

    let receiving = inbox.clone();
    let runtime = tokio::runtime::Handle::current();
    let preview_metrics = receive_metrics.clone();
    let relay = std::thread::spawn(move || {
        let mut frames = 0;
        let mut sequence = 0;
        while let Some(frame) = receiving.next() {
            assert!(screen::bounded_parameter_sets(&frame.data));
            if let Some(preview) = &preview {
                let packets =
                    screen::packetize(frame.data, &mut sequence, frame.timestamp).unwrap();
                let at = Instant::now();
                let sent = runtime.block_on(async {
                    for packet in packets {
                        if !matches!(
                            tokio::time::timeout(Duration::from_millis(50), preview.write(packet))
                                .await,
                            Ok(Ok(_))
                        ) {
                            return false;
                        }
                    }
                    true
                });
                preview_metrics.time("preview_submit", at.elapsed());
                preview_metrics.add(
                    if sent {
                        "preview_frames"
                    } else {
                        "preview_errors"
                    },
                    1,
                );
            }
            frames += 1;
        }
        frames
    });
    let source = super::window::Source::new()?;
    let connection = Connection::default();
    let demand = Arc::new(Demand::default());
    let mut controller = screen::adaptation::Receiver::default();
    demand.update(controller.bitrate());
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = tokio::sync::mpsc::channel(screen::sender_queue_frames(quality.fps));
    let sender = screen::send::Sender {
        local: None,
        network_demand: None,
        connection: connection.clone(),
        video: track,
        demand: demand.clone(),
        force_keyframe: force_keyframe.clone(),
        sequence: Arc::default(),
        metrics: metrics.clone(),
    };
    let send_task = tokio::spawn(sender.run(Arc::new(AtomicBool::new(false)), quality, rx));
    let started = Instant::now();
    let worker = {
        let (stop, connection, metrics, force_keyframe, demand) = (
            stop.clone(),
            connection.clone(),
            metrics.clone(),
            force_keyframe.clone(),
            demand.clone(),
        );
        let id = source.id;
        std::thread::spawn(move || {
            screen::capture::run(
                &SourceId::Window(id),
                quality,
                stop,
                connection,
                started,
                tx,
                metrics,
                force_keyframe,
                demand,
                |message| println!("{message}"),
            )
        })
    };
    let mut previous = 0;
    let mut previous_lost = 0;
    let mut previous_at = Instant::now();
    let mut warm = None;
    for tick in 0..seconds * 2 {
        demand.update(controller.bitrate());
        tokio::time::sleep(Duration::from_millis(500)).await;
        if tick % 2 == 1 {
            let publisher = metrics.snapshot();
            let receiver = receive_metrics.snapshot();
            let frames = receiver.counters["received_frames"];
            println!(
                "4K pipeline: t={:.1}s received_fps={:.2} encoded={} sent={} drops={} resolution={}",
                started.elapsed().as_secs_f64(),
                (frames - previous) as f64 / previous_at.elapsed().as_secs_f64(),
                publisher.counters["encoded_frames"],
                publisher.counters["sent_frames"],
                publisher.counters["encode_dropped"],
                publisher
                    .labels
                    .get("resolution")
                    .map(String::as_str)
                    .unwrap_or("pending")
            );
            let lost = receiver.counters["lost_packets"];
            controller.sample_at(
                screen::adaptation::ReceiverSample {
                    congested: lost > previous_lost,
                    receiving: frames > previous,
                    ..Default::default()
                },
                Instant::now(),
            );
            previous_lost = lost;
            previous = frames;
            previous_at = Instant::now();
            if tick == 9 {
                warm = Some((Instant::now(), frames));
            }
        }
    }
    let sustained = warm.map(|(at, frames)| {
        (receive_metrics.snapshot().counters["received_frames"] - frames) as f64
            / at.elapsed().as_secs_f64()
    });
    stop.store(true, Ordering::Release);
    let capture = worker.join().map_err(|_| "Capture probe panicked")?;
    capture?;
    tokio::time::timeout(Duration::from_secs(2), send_task).await??;
    connection.close();
    drop(source);
    publisher.close().await?;
    receiver.close().await?;
    inbox.close();
    let frames = relay.join().map_err(|_| "Receiver probe panicked")?;
    println!(
        "Publisher diagnostics: {}",
        serde_json::to_string(&metrics.snapshot())?
    );
    println!(
        "Receiver diagnostics: {}",
        serde_json::to_string(&receive_metrics.snapshot())?
    );
    println!("Complete: {frames} assembled 4K frames; sustained_fps={sustained:?}");
    if std::env::args().any(|arg| arg == "--assert") {
        let publisher = metrics.snapshot();
        let receiver = receive_metrics.snapshot();
        if sustained.is_none_or(|fps| fps < 59.0)
            || publisher.labels.get("resolution").map(String::as_str) != Some("3840x2160")
            || publisher
                .labels
                .get("source_resolution")
                .map(String::as_str)
                != Some("3840x2160")
            || publisher.counters["encode_dropped"] != 0
            || receiver.counters["lost_packets"] != 0
            || receiver.counters["preview_errors"] != 0
        {
            return Err(
                "4K60 native pipeline acceptance failed; inspect aggregate diagnostics".into(),
            );
        }
        println!(
            "PASS: sustained native 4K60, zero encoded drops, packet loss or preview errors. Browser presentation must be checked separately while visible."
        );
    }
    Ok(())
}
