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
    fn recover_burst(&self, encoded_bitrate: u32, wire_bytes: usize, rate: u32) {
        // Scale by measured wire size, including fragmentation overhead. This
        // remains effective when hardware ignores the nominal 200 ms HRD hint.
        let allowed = u64::from(rate) * BURST_BUDGET.as_millis() as u64 / 8 / 1000;
        let limit = (u64::from(encoded_bitrate) * allowed / (wire_bytes as u64).max(1))
            .min(u64::from(encoded_bitrate.saturating_sub(1))) as u32;
        self.demand.limit_encoder(limit);
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
            // the shared admission rate; the SFU checks their actual bitrate
            // against each viewer before forwarding negotiated layers.
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
                self.recover_burst(frame.bitrate, wire_bytes, rate);
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
                    self.recover_burst(frame.bitrate, wire_bytes, rate);
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

    #[tokio::test]
    async fn severe_receiver_cuts_recover_both_layers_through_real_sender_and_inbox() {
        use super::super::{
            adaptation::{QualityController, Receiver, ReceiverSample},
            pacing::*,
            receive::Inbox,
        };
        use thiscord_shared::screen::LayerTargets;
        for (maximum, cuts) in [
            (Quality::default(), 5),
            (LayerTargets::low_quality(Quality::default()), 8),
        ] {
            let mut receiver = Receiver::default();
            let mut controller = QualityController::new(maximum);
            let start = Instant::now();
            let mut held = maximum;
            for second in 0..=cuts * 2 {
                let now = start + Duration::from_secs(second);
                receiver.sample_at(
                    ReceiverSample {
                        congested: true,
                        severe: true,
                        receiving: true,
                    },
                    now,
                );
                held = controller.update(receiver.bitrate(), now).unwrap();
            }
            if cuts == 5 {
                assert_eq!(receiver.bitrate(), 3_712_930);
            }
            assert_eq!(
                held, maximum,
                "the reachable trajectory still holds the old mode"
            );
            assert!(held.bitrate() > receiver.bitrate());
            let demand = Arc::new(Demand::default());
            demand.update(receiver.bitrate());
            let other_layer = Demand::default();
            other_layer.update(32_000_000);
            let metrics = Metrics::default();
            let (preview, inbox) = Inbox::channel();
            let preview = Arc::new(preview);
            let sender = Sender {
                local: Some(LocalPreview {
                    sender: preview.clone(),
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
            let mut budget = SendBudget::new(transport_budget(held.bitrate(), receiver.bitrate()));
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            let size = (140_042_u64 * u64::from(held.bitrate()) / 5_000_000) as usize;
            tx.send(outgoing(held, size, true, &budget, 0))
                .await
                .unwrap();
            let job = tokio::spawn(sender.run(Arc::new(AtomicBool::new(false)), maximum, rx));
            tokio::time::timeout(Duration::from_secs(1), async {
                while metrics.snapshot().counters["sender_burst_recovery"] == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                inbox.metrics().snapshot().counters["received_packets"],
                0,
                "reject an impossible burst before corrupting the receiver chain"
            );
            let next = controller
                .update_with_recovery(
                    receiver.bitrate(),
                    demand.take_encoder_limit(),
                    start + Duration::from_secs(cuts * 2),
                )
                .unwrap();
            assert!(
                next.bitrate() < held.bitrate(),
                "deadline recovery bypasses held-mode dwell"
            );
            assert_eq!(other_layer.take_encoder_limit(), 0);
            budget.set_bitrate(transport_budget(next.bitrate(), receiver.bitrate()));
            // The hardware's new SPS/PPS/IDR is followed by dependent output.
            // Real pacing, RTP fragmentation, Inbox assembly and synchronization
            // must all work, not merely satisfy the sender's 350 ms age check.
            let size = (140_042_u64 * u64::from(next.bitrate()) / 5_000_000) as usize;
            tx.send(outgoing(next, size, true, &budget, 9000))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while inbox.metrics().snapshot().counters["received_frames"] == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(inbox.next().unwrap().reset);
            tx.send(outgoing(next, 100, false, &budget, 12000))
                .await
                .unwrap();
            drop(tx);
            job.await.unwrap();
            assert_eq!(inbox.metrics().snapshot().counters["received_frames"], 2);
            assert_eq!(inbox.metrics().snapshot().counters["expired_frames"], 0);
            assert!(
                !inbox.next().unwrap().reset,
                "delta remains decodable after recovery"
            );
            assert_eq!(budget.bytes(), 0);
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
        assert_eq!(demand.take_encoder_limit(), 0);
        assert_eq!(budget.bytes(), 0);
    }
}
