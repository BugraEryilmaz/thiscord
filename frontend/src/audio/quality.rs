//! Bounded audio-specific aggregates. Never call Metrics from a device callback.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use thiscord_shared::audio::{QualityCounts, QualityMetrics, QualityTiming};

const COUNT: usize = 30;
const WINDOWS: usize = 10;
const LIMITS: [u64; 16] = [
    50,
    100,
    250,
    500,
    1000,
    2000,
    4000,
    6000,
    8000,
    10000,
    15000,
    20000,
    30000,
    50000,
    100000,
    u64::MAX,
];
#[derive(Clone, Copy)]
pub enum Count {
    CaptureOverflowSamples,
    ReferenceOverflowSamples,
    InputSamples,
    InputClippedSamples,
    OutputSamples,
    OutputClippedSamples,
    OutgoingOfferedPackets,
    OutgoingFullPackets,
    OutgoingExpiredPackets,
    OutgoingInvalidatedPackets,
    OutgoingSentPackets,
    OutgoingFailedPackets,
    IncomingQueueDroppedPackets,
    ReceivedPackets,
    DuplicatePackets,
    ReorderedPackets,
    LatePackets,
    JitterOverflowPackets,
    ResyncDiscardedPackets,
    InvalidPackets,
    SequenceGapPackets,
    FecAttempts,
    PlcSamples,
    DecodedSamples,
    DecodeErrors,
    PcmOverflowSamples,
    StarvationSamples,
    EmptyUnknownSamples,
    IntentionalSilenceSamples,
    PcmDiscardedSamples,
}
#[derive(Clone, Copy)]
pub enum Stage {
    Capture,
    Render,
    Encode,
    Decode,
}
impl Stage {
    fn budget(self) -> u64 {
        match self {
            Self::Capture | Self::Render => 10_000,
            _ => 20_000,
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Histogram {
    bins: [u64; 16],
    calls: u64,
    misses: u64,
    max: u64,
}
impl Histogram {
    fn record(&mut self, us: u64, budget: u64) {
        let index = LIMITS.iter().position(|limit| us <= *limit).unwrap_or(15);
        self.bins[index] = self.bins[index].saturating_add(1);
        self.calls = self.calls.saturating_add(1);
        self.misses = self.misses.saturating_add(u64::from(us > budget));
        self.max = self.max.max(us);
    }
    fn merge(&mut self, other: &Self) {
        for (a, b) in self.bins.iter_mut().zip(other.bins) {
            *a = a.saturating_add(b);
        }
        self.calls = self.calls.saturating_add(other.calls);
        self.misses = self.misses.saturating_add(other.misses);
        self.max = self.max.max(other.max);
    }
    fn percentile(&self, p: u64) -> Option<u64> {
        if self.calls == 0 {
            return None;
        }
        let rank = (u128::from(self.calls) * u128::from(p)).div_ceil(100);
        let mut count = 0u128;
        for (i, n) in self.bins.iter().enumerate() {
            count += u128::from(*n);
            if count >= rank {
                return Some(LIMITS[i].min(self.max));
            }
        }
        Some(self.max)
    }
    fn snapshot(&self) -> QualityTiming {
        QualityTiming {
            calls: self.calls,
            deadline_misses: self.misses,
            p50_us: self.percentile(50),
            p95_us: self.percentile(95),
            p99_us: self.percentile(99),
            max_us: (self.calls > 0).then_some(self.max),
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Bucket {
    second: Option<u64>,
    counts: [u64; COUNT],
    timings: [Histogram; 4],
}
struct Window {
    total: [u64; COUNT],
    buckets: [Bucket; WINDOWS],
    second: u64,
}
impl Default for Window {
    fn default() -> Self {
        Self {
            total: [0; COUNT],
            buckets: [Bucket::default(); WINDOWS],
            second: 0,
        }
    }
}
impl Window {
    fn bucket(&mut self, second: u64) -> &mut Bucket {
        // A backwards injected clock cannot revive expired buckets.
        self.second = self.second.max(second);
        let b = &mut self.buckets[(self.second % WINDOWS as u64) as usize];
        if b.second != Some(self.second) {
            *b = Bucket {
                second: Some(self.second),
                ..Default::default()
            };
        }
        b
    }
    fn add(&mut self, second: u64, event: Count, n: u64) {
        self.total[event as usize] = self.total[event as usize].saturating_add(n);
        let b = self.bucket(second);
        b.counts[event as usize] = b.counts[event as usize].saturating_add(n);
    }
    fn snapshot(&mut self, elapsed: Duration) -> QualityMetrics {
        self.bucket(elapsed.as_secs());
        let mut recent = [0u64; COUNT];
        let mut hist = [Histogram::default(); 4];
        for b in &self.buckets {
            if b.second
                .is_some_and(|s| self.second.saturating_sub(s) < WINDOWS as u64)
            {
                for (a, v) in recent.iter_mut().zip(b.counts) {
                    *a = a.saturating_add(v);
                }
                for (a, v) in hist.iter_mut().zip(b.timings) {
                    a.merge(&v);
                }
            }
        }
        QualityMetrics {
            total: counts(self.total),
            recent: counts(recent),
            window_ms: elapsed
                .as_millis()
                .min(9000 + elapsed.subsec_millis() as u128) as u64,
            capture: hist[0].snapshot(),
            render: hist[1].snapshot(),
            encode: hist[2].snapshot(),
            decode: hist[3].snapshot(),
        }
    }
}
fn counts(v: [u64; COUNT]) -> QualityCounts {
    QualityCounts {
        capture_overflow_samples: v[Count::CaptureOverflowSamples as usize],
        reference_overflow_samples: v[Count::ReferenceOverflowSamples as usize],
        input_samples: v[Count::InputSamples as usize],
        input_clipped_samples: v[Count::InputClippedSamples as usize],
        output_samples: v[Count::OutputSamples as usize],
        output_clipped_samples: v[Count::OutputClippedSamples as usize],
        outgoing_offered_packets: v[Count::OutgoingOfferedPackets as usize],
        outgoing_full_packets: v[Count::OutgoingFullPackets as usize],
        outgoing_expired_packets: v[Count::OutgoingExpiredPackets as usize],
        outgoing_invalidated_packets: v[Count::OutgoingInvalidatedPackets as usize],
        outgoing_sent_packets: v[Count::OutgoingSentPackets as usize],
        outgoing_failed_packets: v[Count::OutgoingFailedPackets as usize],
        incoming_queue_dropped_packets: v[Count::IncomingQueueDroppedPackets as usize],
        received_packets: v[Count::ReceivedPackets as usize],
        duplicate_packets: v[Count::DuplicatePackets as usize],
        reordered_packets: v[Count::ReorderedPackets as usize],
        late_packets: v[Count::LatePackets as usize],
        jitter_overflow_packets: v[Count::JitterOverflowPackets as usize],
        resync_discarded_packets: v[Count::ResyncDiscardedPackets as usize],
        invalid_packets: v[Count::InvalidPackets as usize],
        sequence_gap_packets: v[Count::SequenceGapPackets as usize],
        fec_attempts: v[Count::FecAttempts as usize],
        plc_samples: v[Count::PlcSamples as usize],
        decoded_samples: v[Count::DecodedSamples as usize],
        decode_errors: v[Count::DecodeErrors as usize],
        pcm_overflow_samples: v[Count::PcmOverflowSamples as usize],
        starvation_samples: v[Count::StarvationSamples as usize],
        empty_unknown_samples: v[Count::EmptyUnknownSamples as usize],
        intentional_silence_samples: v[Count::IntentionalSilenceSamples as usize],
        pcm_discarded_samples: v[Count::PcmDiscardedSamples as usize],
    }
}
/// Fixed ten-second storage, owned by a connection/session or a single source.
pub struct Metrics {
    start: Instant,
    window: Mutex<Window>,
    network: Mutex<Option<(Instant, HopStats)>>,
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            window: Mutex::new(Window::default()),
            network: Mutex::new(None),
        }
    }
}
impl Metrics {
    pub fn add(&self, event: Count, n: u64) {
        if n == 0 {
            return;
        }
        if let Ok(mut w) = self.window.lock() {
            w.add(self.start.elapsed().as_secs(), event, n);
        }
    }
    pub fn duration(&self, stage: Stage, elapsed: Duration) {
        if let Ok(mut w) = self.window.lock() {
            w.bucket(self.start.elapsed().as_secs()).timings[stage as usize].record(
                elapsed.as_micros().min(u128::from(u64::MAX)) as u64,
                stage.budget(),
            );
        }
    }
    pub fn timed(&self, stage: Stage) -> Timer<'_> {
        Timer {
            metrics: self,
            stage,
            start: Instant::now(),
        }
    }
    pub fn snapshot(&self) -> QualityMetrics {
        self.window
            .lock()
            .map(|mut w| w.snapshot(self.start.elapsed()))
            .unwrap_or_default()
    }
    pub fn set_network(&self, value: HopStats) {
        if let Ok(mut network) = self.network.lock() {
            *network = Some((Instant::now(), value));
        }
    }
    pub fn network(&self) -> HopStats {
        self.network_at(Instant::now())
    }
    fn network_at(&self, now: Instant) -> HopStats {
        self.network
            .lock()
            .ok()
            .and_then(|network| {
                network
                    .filter(|(at, _)| now.saturating_duration_since(*at) < Duration::from_secs(3))
                    .map(|(_, v)| v)
            })
            .unwrap_or_default()
    }
}
pub struct Timer<'a> {
    metrics: &'a Metrics,
    stage: Stage,
    start: Instant,
}
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        self.metrics.duration(self.stage, self.start.elapsed());
    }
}

/// Only numeric, hop-scoped fields; no addresses or report/transport identifiers.
#[derive(Clone, Copy, Default)]
pub struct HopStats {
    pub rtt_ms: Option<f64>,
    pub received: Option<u64>,
    pub lost: Option<i64>,
}
pub fn network(report: &rtc::statistics::report::RTCStatsReport) -> HopStats {
    use rtc::{
        rtp_transceiver::rtp_sender::RtpCodecKind, statistics::report::RTCStatsReportEntry as Entry,
    };
    let mut result = HopStats::default();
    for entry in report.iter() {
        match entry {
            Entry::IceCandidatePair(s) if s.nominated && s.responses_received > 0 => {
                let ms = s.current_round_trip_time * 1000.0;
                result.rtt_ms = (ms.is_finite() && ms >= 0.0).then_some(ms);
            }
            Entry::InboundRtp(s)
                if s.received_rtp_stream_stats.rtp_stream_stats.kind == RtpCodecKind::Audio =>
            {
                let stats = &s.received_rtp_stream_stats;
                result.received = Some(
                    result
                        .received
                        .unwrap_or(0)
                        .saturating_add(stats.packets_received),
                );
                result.lost = Some(result.lost.unwrap_or(0).saturating_add(stats.packets_lost));
            }
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_expire_without_events_and_totals_saturate_without_wrapping() {
        let mut w = Window::default();
        w.add(0, Count::ReceivedPackets, 7);
        w.add(9, Count::ReceivedPackets, 2);
        assert_eq!(
            w.snapshot(Duration::from_millis(9500))
                .recent
                .received_packets,
            9
        );
        let s = w.snapshot(Duration::from_secs(10));
        assert_eq!(s.recent.received_packets, 2);
        assert_eq!(s.window_ms, 9000);
        assert_eq!(
            w.snapshot(Duration::from_secs(19)).recent.received_packets,
            0
        );
        assert_eq!(
            w.snapshot(Duration::from_secs(500)).total.received_packets,
            9
        );
        w.add(501, Count::ReceivedPackets, u64::MAX);
        w.add(501, Count::ReceivedPackets, 1);
        assert_eq!(
            w.snapshot(Duration::from_secs(501)).total.received_packets,
            u64::MAX
        );
        assert_eq!(
            w.snapshot(Duration::from_secs(501)).recent.received_packets,
            u64::MAX
        );
        // A backwards clock must not resurrect old observations.
        assert_eq!(
            w.snapshot(Duration::from_secs(0)).recent.received_packets,
            u64::MAX
        );
        assert_eq!(
            Window::default()
                .snapshot(Duration::ZERO)
                .total
                .received_packets,
            0
        );
    }
    #[test]
    fn histogram_bounds_percentiles_deadlines_and_expiry_are_exact() {
        let mut w = Window::default();
        for i in 1..=100 {
            w.bucket(0).timings[0].record(i * 100, 10_000);
        }
        let t = w.snapshot(Duration::from_secs(1)).capture;
        assert_eq!(
            (t.p50_us, t.p95_us, t.p99_us),
            (Some(6000), Some(10000), Some(10000))
        );
        assert_eq!(t.deadline_misses, 0);
        w.bucket(0).timings[0].record(10_001, 10_000);
        w.bucket(0).timings[0].record(u64::MAX, 10_000);
        assert_eq!(
            w.snapshot(Duration::from_secs(1)).capture.deadline_misses,
            2
        );
        assert_eq!(
            w.snapshot(Duration::from_secs(1)).capture.max_us,
            Some(u64::MAX)
        );
        assert_eq!(w.snapshot(Duration::from_secs(11)).capture.p50_us, None);
        let mut h = Histogram {
            calls: u64::MAX,
            bins: [u64::MAX; 16],
            ..Default::default()
        };
        h.record(u64::MAX, 0);
        assert_eq!(h.calls, u64::MAX);
        assert!(h.percentile(99).is_some());
    }
    #[test]
    fn hop_values_expire_and_reconnect_has_no_previous_measurements() {
        let connection = super::super::connection::Connection::default();
        connection.metrics.set_network(HopStats {
            rtt_ms: Some(24.0),
            received: Some(50),
            lost: Some(2),
        });
        connection
            .metrics
            .add(Count::IncomingQueueDroppedPackets, 2);
        assert_eq!(connection.metrics.network().rtt_ms, Some(24.0));
        assert!(
            connection
                .metrics
                .network_at(Instant::now() + Duration::from_secs(3))
                .rtt_ms
                .is_none()
        );
        let next = super::super::connection::Connection::default();
        assert_eq!(
            next.metrics.snapshot().total.incoming_queue_dropped_packets,
            0
        );
        assert!(next.metrics.network().received.is_none());
    }
    #[test]
    fn eight_participants_and_all_slots_have_fixed_storage_under_saturation() {
        assert!(std::mem::size_of::<Metrics>() < 16 * 1024);
        let streams: Vec<_> = (0..super::super::mixer::MAX_STREAMS)
            .map(|_| Metrics::default())
            .collect();
        let start = Instant::now();
        // One minute, 50 packets/sec, at the supported room capacity.
        for tick in 0..3000 {
            for (slot, m) in streams
                .iter()
                .enumerate()
                .take(thiscord_shared::voice::ROOM_CAPACITY)
            {
                m.add(Count::ReceivedPackets, 1);
                m.duration(Stage::Decode, Duration::from_micros((tick + slot) as u64));
                if tick % 50 == 0 {
                    std::hint::black_box(m.snapshot());
                }
            }
        }
        for m in streams.iter().take(thiscord_shared::voice::ROOM_CAPACITY) {
            assert_eq!(m.snapshot().total.received_packets, 3000);
        }
        for m in streams.iter().skip(thiscord_shared::voice::ROOM_CAPACITY) {
            assert_eq!(m.snapshot().total.received_packets, 0);
        }
        eprintln!(
            "Quality instrumentation: 24000 packet/timing pairs in {:?}; {} bytes per source (informational)",
            start.elapsed(),
            std::mem::size_of::<Metrics>()
        );
    }
}
