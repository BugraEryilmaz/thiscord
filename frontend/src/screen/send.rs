//! Publisher pacing and cancellation, shared by the desktop and pipeline probes.
use super::{Outgoing, adaptation::Demand, metrics::Metrics};
use crate::audio::connection::Connection;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::Quality;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;

// Reserve 50 ms of the receiver's 250 ms assembly window for scheduler/network
// variation. In-flight cuts get a 20 ms margin; neither receiver nor queue ages
// are extended. An impossible burst requests a smaller recovery encoder mode.
const BURST_BUDGET: Duration = Duration::from_millis(200);
const BURST_DEADLINE: Duration = Duration::from_millis(230);
fn burst_duration(bytes: usize, bitrate: u32) -> Duration {
    Duration::from_secs_f64(bytes as f64 * 8.0 / f64::from(bitrate.max(1)))
}

pub struct LocalPreview {
    pub sender: Arc<super::receive::Sender>,
    pub epoch: Arc<AtomicU32>,
    pub visible: Arc<AtomicBool>,
}
pub struct Sender {
    pub local: Option<LocalPreview>,
    /// Negotiated screen source fence, including encoder output queued before restart.
    pub source_epoch: Option<Arc<AtomicU32>>,
    pub connection: Connection,
    pub video: Arc<TrackLocalStaticRTP>,
    pub demand: Arc<Demand>,
    pub network_demand: Option<Arc<Demand>>,
    pub force_keyframe: Arc<AtomicBool>,
    pub sequence: Arc<AtomicU32>,
    pub metrics: Metrics,
}
impl Sender {
    fn recover_burst(&self, encoded_bitrate: u32, keyframe: bool, wire_bytes: usize, rate: u32) {
        self.demand.reject_encoder_frame(encoded_bitrate, keyframe);
        self.metrics.label("rejected_burst_wire_bytes", wire_bytes);
        self.metrics.label("rejected_burst_wire_bitrate", rate);
        self.metrics
            .label("rejected_burst_encoder_bitrate", encoded_bitrate);
        self.force_keyframe.store(true, Ordering::Release);
        self.metrics.add("encode_dropped", 1);
        self.metrics.add("sender_burst_recovery", 1);
    }
    pub async fn run(
        self,
        stop: Arc<AtomicBool>,
        quality: Quality,
        mut rx: tokio::sync::mpsc::Receiver<Outgoing>,
    ) {
        let mut pacer =
            super::pacing::Pacer::new(super::pacing::transport_bitrate(quality.bitrate()));
        let mut waiting = false;
        'frames: while let Some(mut frame) = rx.recv().await {
            let epoch = self
                .source_epoch
                .as_ref()
                .map(|e| e.load(Ordering::Acquire));
            let budget = self.demand.bitrate();
            // A held encoder can temporarily exceed demand. Pace its frames with
            // live demand. Nominal metadata remains truthful; the SFU must police
            // measured wire bytes rather than reject a held encoder's metadata.
            if budget == 0 {
                waiting = true;
                self.force_keyframe.store(true, Ordering::Release);
                continue;
            }
            let captured_at = frame.captured_at;
            self.metrics
                .time("sender_queue_wait", frame.enqueued_at.elapsed());
            let stale = captured_at.elapsed() > super::SENDER_QUEUE_AGE;
            if stale || (waiting && !frame.keyframe) {
                self.metrics.add(
                    if stale {
                        "sender_stale"
                    } else {
                        "sender_dependent"
                    },
                    1,
                );
                self.metrics.add("encode_dropped", 1);
                self.metrics
                    .event("paced sender dropped stale or dependent frame");
                if !waiting || frame.keyframe {
                    self.force_keyframe.store(true, Ordering::Release);
                }
                waiting = true;
                continue;
            }
            let wire_bytes: usize = frame.packets.iter().map(|p| p.payload.len() + 64).sum();
            let rate = frame.budget.pacing_bitrate(budget);
            if burst_duration(wire_bytes, rate) > BURST_BUDGET {
                self.recover_burst(frame.bitrate, frame.keyframe, wire_bytes, rate);
                waiting = true;
                continue;
            }
            let send_started = Instant::now();
            let mut remaining = wire_bytes;
            self.metrics
                .peak("sender_peak_frame_packets", frame.packets.len() as u64);
            for mut packet in frame.packets {
                let bytes = packet.payload.len() + 64;
                let demand = self.demand.bitrate();
                if demand == 0 {
                    waiting = true;
                    self.force_keyframe.store(true, Ordering::Release);
                    continue 'frames;
                }
                let now = Instant::now();
                let rate = frame.budget.pacing_bitrate(demand);
                if send_started.elapsed() + burst_duration(remaining, rate) > BURST_DEADLINE {
                    self.recover_burst(frame.bitrate, frame.keyframe, wire_bytes, rate);
                    waiting = true;
                    continue 'frames;
                }
                pacer.set_bitrate(rate, now);
                let at = pacer.reserve(now, bytes);
                if at > Instant::now() {
                    let sleep_started = Instant::now();
                    self.metrics.add("pacing_sleeps", 1);
                    tokio::time::sleep_until(at.into()).await;
                    self.metrics.time("pacing_wait", sleep_started.elapsed());
                    self.metrics.time(
                        "pacing_lateness",
                        Instant::now().saturating_duration_since(at),
                    );
                }
                if stop.load(Ordering::Acquire) || !self.connection.active() {
                    return;
                }
                if self.demand.bitrate() == 0
                    || self
                        .source_epoch
                        .as_ref()
                        .is_some_and(|e| Some(e.load(Ordering::Acquire)) != epoch)
                {
                    waiting = true;
                    self.force_keyframe.store(true, Ordering::Release);
                    continue 'frames;
                }
                if send_started.elapsed() >= BURST_DEADLINE {
                    self.recover_burst(
                        frame.bitrate,
                        frame.keyframe,
                        wire_bytes,
                        frame.budget.pacing_bitrate(self.demand.bitrate()),
                    );
                    waiting = true;
                    continue 'frames;
                }
                remaining -= bytes;
                if captured_at.elapsed() > Duration::from_millis(350) {
                    waiting = true;
                    self.force_keyframe.store(true, Ordering::Release);
                    self.metrics.add("encode_dropped", 1);
                    self.metrics.add("sender_stale", 1);
                    self.metrics
                        .event("paced sender dropped stale or dependent frame");
                    continue 'frames;
                }
                packet.header.sequence_number =
                    self.sequence.fetch_add(1, Ordering::Relaxed) as u16;
                if let Some(local) = &self.local
                    && local.visible.load(Ordering::Acquire)
                {
                    let mut preview = packet.clone();
                    preview.header.csrc = vec![local.epoch.load(Ordering::Acquire)];
                    local.sender.push(preview, Instant::now());
                    if local.sender.keyframe_request(Instant::now()).is_some() {
                        self.force_keyframe.store(true, Ordering::Release);
                    }
                }
                if self
                    .network_demand
                    .as_ref()
                    .is_some_and(|d| d.bitrate() == 0)
                {
                    frame.budget.sent(bytes);
                    continue;
                }
                if let Some(epoch) = epoch {
                    packet.header.csrc = thiscord_shared::screen::LayerPacket {
                        epoch,
                        bitrate: frame.bitrate,
                    }
                    .csrc();
                }
                let write_started = Instant::now();
                let write_budget = BURST_DEADLINE
                    .saturating_sub(send_started.elapsed())
                    .min(Duration::from_millis(100));
                let result = tokio::time::timeout(write_budget, self.video.write_rtp(packet)).await;
                self.metrics.time("rtp_write", write_started.elapsed());
                if !matches!(result, Ok(Ok(_))) {
                    self.metrics.add(
                        if result.is_err() {
                            "send_timeouts"
                        } else {
                            "send_errors"
                        },
                        1,
                    );
                    waiting = true;
                    self.force_keyframe.store(true, Ordering::Release);
                    self.metrics.add("encode_dropped", 1);
                    self.metrics
                        .event("paced sender dropped stale or dependent frame");
                    continue 'frames;
                }
                self.metrics.add("sent_packets", 1);
                self.metrics.add("sent_bytes", bytes as u64);
                frame.budget.sent(bytes);
            }
            waiting = false;
            self.demand.delivered_encoder_frame(frame.bitrate);
            self.metrics.time("frame_send", send_started.elapsed());
            self.metrics.add("sent_frames", 1);
            self.metrics.time("send_age", captured_at.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outgoing(
        quality: Quality,
        size: usize,
        key: bool,
        budget: &super::super::pacing::SendBudget,
        timestamp: u32,
    ) -> Outgoing {
        let mut data = if key {
            vec![0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
        } else {
            vec![0, 0, 1, 0x61, 1]
        };
        data.resize(size, 1);
        let packets = super::super::packetize(data, &mut 0, timestamp).unwrap();
        let bytes = packets.iter().map(|p| p.payload.len() + 64).sum();
        Outgoing {
            bitrate: quality.bitrate(),
            captured_at: Instant::now(),
            enqueued_at: Instant::now(),
            packets,
            keyframe: key,
            budget: budget.track(bytes),
        }
    }

    // These sizes come from the NVIDIA cold/warm reproduction, deliberately
    // without proportional scaling by nominal codec bitrate.
    #[tokio::test]
    async fn non_proportional_idrs_recover_both_layers_without_shrinking_wire_capacity() {
        use super::super::{adaptation::QualityController, pacing::*, receive::Inbox};
        use thiscord_shared::screen::LayerTargets;
        for maximum in [
            Quality::default(),
            LayerTargets::low_quality(Quality::default()),
        ] {
            for (rate, sizes, expected_rejected, expected_quality) in [
                // The reachable five-severe-cut held-1080p case also retries
                // non-proportional bursts before selecting a smaller mode.
                (
                    3_712_930,
                    vec![140_042, 140_042, 140_042, 90_000],
                    3,
                    Quality {
                        height: 1080,
                        fps: 15,
                    },
                ),
                (
                    1_250_000,
                    vec![40_758, 24_263],
                    1,
                    Quality {
                        height: 720,
                        fps: 15,
                    },
                ),
                // Two warm retries per mode, including a lower-FPS IDR that
                // grows rather than shrinks. The final 6287-byte floor IDR must
                // keep using 1.25 Mbit/s, not its nominal 166666 bit/s.
                (
                    1_250_000,
                    vec![
                        40_758, 40_758, 40_758, 131_017, 50_000, 40_000, 77_899, 6287,
                    ],
                    7,
                    Quality {
                        height: 360,
                        fps: 5,
                    },
                ),
                // At minimum demand even 6287 bytes cannot fit the unchanged
                // deadline. Retry the warmed floor encoder instead of cycling up.
                (
                    200_000,
                    vec![77_899, 6287, 6287, 50_165, 6429, 4297],
                    5,
                    Quality {
                        height: 360,
                        fps: 5,
                    },
                ),
            ] {
                if rate == 3_712_930 && maximum.height != 1080 {
                    continue;
                }
                let demand = Arc::new(Demand::default());
                let other_layer = Demand::default();
                let metrics = Metrics::default();
                let (preview, inbox) = Inbox::channel();
                let sender = Sender {
                    local: Some(LocalPreview {
                        sender: Arc::new(preview),
                        epoch: Arc::new(AtomicU32::new(7)),
                        visible: Arc::new(AtomicBool::new(true)),
                    }),
                    source_epoch: Some(Arc::new(AtomicU32::new(7))),
                    connection: Connection::default(),
                    video: super::super::track(901),
                    demand: demand.clone(),
                    network_demand: Some(Arc::new(Demand::default())),
                    force_keyframe: Arc::new(AtomicBool::new(false)),
                    sequence: Default::default(),
                    metrics: metrics.clone(),
                };
                let (tx, rx) = tokio::sync::mpsc::channel(4);
                let job = tokio::spawn(sender.run(Arc::new(AtomicBool::new(false)), maximum, rx));
                let mut controller = QualityController::new(maximum);
                let mut budget = SendBudget::new(1);
                let start = Instant::now();
                let mut quality = maximum;
                if rate == 3_712_930 {
                    controller.update(maximum.bitrate(), start);
                }
                for (tick, size) in sizes.into_iter().enumerate() {
                    demand.update(rate);
                    quality = controller
                        .update_with_recovery(
                            rate,
                            demand.take_encoder_feedback(),
                            start + Duration::from_millis(tick as u64 * 200),
                        )
                        .unwrap();
                    budget.set_bitrate(transport_budget(quality.bitrate(), rate));
                    assert_eq!(
                        budget.track(0).pacing_bitrate(rate),
                        transport_bitrate(rate)
                    );
                    tx.send(outgoing(quality, size, true, &budget, tick as u32 * 18000))
                        .await
                        .unwrap();
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while budget.bytes() != 0 {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    })
                    .await
                    .unwrap();
                }
                assert_eq!(quality, expected_quality);
                assert_eq!(
                    controller.scale_percent(),
                    if rate == 200_000 { 80 } else { 100 }
                );
                assert_eq!(
                    metrics.snapshot().counters["sender_burst_recovery"],
                    expected_rejected
                );
                assert_eq!(inbox.metrics().snapshot().counters["received_frames"], 1);
                assert!(inbox.next().unwrap().reset);
                // A complete recovery preserves the dependent chain.
                tx.send(outgoing(quality, 100, false, &budget, 900000))
                    .await
                    .unwrap();
                drop(tx);
                job.await.unwrap();
                assert_eq!(inbox.metrics().snapshot().counters["received_frames"], 2);
                assert_eq!(inbox.metrics().snapshot().counters["expired_frames"], 0);
                assert!(!inbox.next().unwrap().reset);
                assert_eq!(budget.bytes(), 0);
                assert!(other_layer.take_encoder_feedback().is_empty());
                assert!(
                    metrics
                        .snapshot()
                        .labels
                        .contains_key("rejected_burst_wire_bytes")
                );
            }
        }
    }

    #[tokio::test]
    async fn held_encoder_burst_survives_small_demand_dip_through_real_inbox() {
        let quality = Quality::default();
        let demand = Arc::new(Demand::default());
        demand.update(4_900_000);
        let metrics = Metrics::default();
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let (preview, inbox) = super::super::receive::Inbox::channel();
        let sender = Sender {
            local: Some(LocalPreview {
                sender: Arc::new(preview),
                epoch: Arc::new(AtomicU32::new(7)),
                visible: Arc::new(AtomicBool::new(true)),
            }),
            source_epoch: Some(Arc::new(AtomicU32::new(7))),
            connection: Connection::default(),
            video: super::super::track(901),
            demand: demand.clone(),
            network_demand: Some(Arc::new(Demand::default())),
            force_keyframe,
            sequence: Default::default(),
            metrics: metrics.clone(),
        };
        let budget = super::super::pacing::SendBudget::new(super::super::pacing::transport_budget(
            quality.bitrate(),
            demand.bitrate(),
        ));
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(outgoing(quality, 132_000, true, &budget, 0))
            .unwrap();
        drop(tx);
        sender
            .run(Arc::new(AtomicBool::new(false)), quality, rx)
            .await;
        assert_eq!(metrics.snapshot().counters["sent_frames"], 1);
        assert_eq!(metrics.snapshot().counters["sender_burst_recovery"], 0);
        assert_eq!(inbox.metrics().snapshot().counters["received_frames"], 1);
        assert!(inbox.next().unwrap().reset);
        assert!(!demand.take_encoder_feedback().has_rejection());
        assert_eq!(budget.bytes(), 0);
    }
}
