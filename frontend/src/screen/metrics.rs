//! Cheap, bounded aggregate diagnostics; no media or identifying data.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use thiscord_shared::screen::{Diagnostics, Timing};
const COUNTERS: &[&str] = &[
    "capture_events",
    "capture_throttled",
    "raw_replaced",
    "encoded_frames",
    "encoded_bytes",
    "keyframes",
    "encode_dropped",
    "encode_backpressure",
    "encode_queue_full",
    "encode_stale",
    "encode_dependent",
    "sender_stale",
    "sender_dependent",
    "send_timeouts",
    "send_errors",
    "pacing_sleeps",
    "encoded_peak_frame_bytes",
    "sender_peak_frame_packets",
    "sent_frames",
    "sent_packets",
    "sent_bytes",
    "received_packets",
    "received_bytes",
    "reordered",
    "late_or_duplicate",
    "lost_packets",
    "queue_resets",
    "expired_frames",
    "received_frames",
    "preview_frames",
    "preview_errors",
    "keyframe_requests",
    "keyframe_feedback",
    "gpu_busy",
    "viewer_connections",
    "viewer_expirations",
    "reorder_peak_packets",
    "receive_peak_frames",
    "receive_peak_bytes",
    "encoder_peak_pending",
    "sender_peak_frames",
];
const TIMINGS: &[&str] = &[
    "capture_submit",
    "gpu_submit",
    "encode_latency",
    "send_age",
    "sender_queue_wait",
    "pacing_wait",
    "pacing_lateness",
    "rtp_write",
    "frame_send",
    "assembly",
    "preview_submit",
];
struct Inner {
    started: Instant,
    counters: Vec<AtomicU64>,
    timings: Vec<[AtomicU64; 3]>,
    events: std::sync::Mutex<std::collections::VecDeque<(u64, String)>>,
    labels: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
}
#[derive(Clone)]
pub struct Metrics(Arc<Inner>);
impl Default for Metrics {
    fn default() -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            counters: COUNTERS.iter().map(|_| AtomicU64::new(0)).collect(),
            timings: TIMINGS
                .iter()
                .map(|_| std::array::from_fn(|_| AtomicU64::new(0)))
                .collect(),
            labels: Default::default(),
            events: Default::default(),
        }))
    }
}
impl Metrics {
    pub fn event(&self, reason: &str) {
        let mut events = self.0.events.lock().unwrap();
        if events.len() == 64 {
            events.pop_front();
        }
        events.push_back((
            self.0.started.elapsed().as_millis() as u64,
            reason.to_owned(),
        ));
    }
    pub fn peak(&self, name: &str, value: u64) {
        if let Some(i) = COUNTERS.iter().position(|n| *n == name) {
            self.0.counters[i].fetch_max(value, Ordering::Relaxed);
        }
    }

    pub fn label(&self, name: &str, value: impl ToString) {
        self.0
            .labels
            .lock()
            .unwrap()
            .insert(name.to_owned(), value.to_string());
    }
    pub fn add(&self, name: &str, value: u64) {
        if let Some(i) = COUNTERS.iter().position(|n| *n == name) {
            self.0.counters[i].fetch_add(value, Ordering::Relaxed);
        }
    }
    pub fn time(&self, name: &str, duration: Duration) {
        if let Some(i) = TIMINGS.iter().position(|n| *n == name) {
            let us = duration.as_micros().min(u64::MAX as u128) as u64;
            self.0.timings[i][0].fetch_add(1, Ordering::Relaxed);
            self.0.timings[i][1].fetch_add(us, Ordering::Relaxed);
            self.0.timings[i][2].fetch_max(us, Ordering::Relaxed);
        }
    }
    pub fn snapshot(&self) -> Diagnostics {
        Diagnostics {
            events: self.0.events.lock().unwrap().iter().cloned().collect(),
            labels: self.0.labels.lock().unwrap().clone(),
            elapsed_ms: self.0.started.elapsed().as_millis() as u64,
            counters: COUNTERS
                .iter()
                .enumerate()
                .map(|(i, n)| (n.to_string(), self.0.counters[i].load(Ordering::Relaxed)))
                .collect(),
            timings: TIMINGS
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    (
                        n.to_string(),
                        Timing {
                            count: self.0.timings[i][0].load(Ordering::Relaxed),
                            total_us: self.0.timings[i][1].load(Ordering::Relaxed),
                            max_us: self.0.timings[i][2].load(Ordering::Relaxed),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Keep only diagnostic scalars from native WebRTC. Never expose raw reports
/// (they contain candidate addresses, certificates and transport identifiers).
pub fn network(
    report: &rtc::statistics::report::RTCStatsReport,
) -> std::collections::BTreeMap<String, String> {
    use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
    use rtc::statistics::report::RTCStatsReportEntry as Entry;
    let mut labels = std::collections::BTreeMap::new();
    for entry in report.iter() {
        let (prefix, value, fields): (String, _, &[&str]) = match entry {
            Entry::IceCandidatePair(s) if s.nominated => (
                "network".into(),
                serde_json::to_value(s),
                &[
                    "currentRoundTripTime",
                    "availableOutgoingBitrate",
                    "availableIncomingBitrate",
                    "packetsDiscardedOnSend",
                    "state",
                ],
            ),
            Entry::InboundRtp(s)
                if s.received_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Video =>
            {
                (
                    format!(
                        "network_rx_{}",
                        s.received_rtp_stream_stats.rtp_stream_stats.ssrc
                    ),
                    serde_json::to_value(s),
                    &[
                        "packetsReceived",
                        "packetsLost",
                        "jitter",
                        "bytesReceived",
                        "nackCount",
                        "pliCount",
                    ],
                )
            }
            Entry::RemoteInboundRtp(s)
                if s.received_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Video =>
            {
                (
                    "network_remote_receiver".into(),
                    serde_json::to_value(s),
                    &["packetsLost", "jitter", "fractionLost", "roundTripTime"],
                )
            }
            _ => continue,
        };
        if let Ok(value) = value {
            for &field in fields {
                if let Some(v) = value.get(field) {
                    labels.insert(format!("{prefix}_{field}"), v.to_string());
                }
            }
        }
    }
    labels
}
