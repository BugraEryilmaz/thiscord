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

pub struct LocalPreview {
    pub sender: Arc<super::receive::Sender>,
    pub epoch: Arc<AtomicU32>,
    pub visible: Arc<AtomicBool>,
}
pub struct Sender {
    pub local: Option<LocalPreview>,
    pub connection: Connection,
    pub video: Arc<TrackLocalStaticRTP>,
    pub demand: Arc<Demand>,
    pub network_demand: Option<Arc<Demand>>,
    pub force_keyframe: Arc<AtomicBool>,
    pub sequence: Arc<AtomicU32>,
    pub metrics: Metrics,
}
impl Sender {
    pub async fn run(
        self,
        stop: Arc<AtomicBool>,
        quality: Quality,
        mut rx: tokio::sync::mpsc::Receiver<Outgoing>,
    ) {
        let mut pacer =
            super::pacing::Pacer::new(super::pacing::transport_bitrate(quality.bitrate()));
        let mut waiting = false;
        let mut paced_quality = quality;
        'frames: while let Some(mut frame) = rx.recv().await {
            let budget = self.demand.bitrate();
            if budget == 0 {
                waiting = true;
                continue;
            }
            let current = quality.constrained(budget);
            if current != paced_quality {
                pacer =
                    super::pacing::Pacer::new(super::pacing::transport_bitrate(current.bitrate()));
                paced_quality = current;
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
            let send_started = Instant::now();
            self.metrics
                .peak("sender_peak_frame_packets", frame.packets.len() as u64);
            for mut packet in frame.packets {
                let bytes = packet.payload.len() + 64;
                let at = pacer.reserve(Instant::now(), bytes);
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
                if self.demand.bitrate() == 0 {
                    waiting = true;
                    self.force_keyframe.store(true, Ordering::Release);
                    continue 'frames;
                }
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
                let write_started = Instant::now();
                let result =
                    tokio::time::timeout(Duration::from_millis(100), self.video.write_rtp(packet))
                        .await;
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
