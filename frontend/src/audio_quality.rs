//! Local presentation shared by the WASM view and native presentation tests.
use std::fmt::Write;
use thiscord_shared::audio::{AudioQuality, QualityMetrics, QualityTiming};

pub fn summary(value: Option<&AudioQuality>) -> String {
    let Some(q) = value else {
        return "Voice quality unavailable".into();
    };
    let c = &q.metrics.recent;
    let mut causes = Vec::new();
    if q.metrics.capture.deadline_misses > 0
        || q.metrics.render.deadline_misses > 0
        || q.metrics.encode.deadline_misses > 0
        || q.metrics.decode.deadline_misses > 0
        || q.streams
            .iter()
            .any(|s| s.metrics.decode.deadline_misses > 0)
    {
        causes.push("Local processing exceeded its time budget");
    }
    if c.capture_overflow_samples > 0
        || c.reference_overflow_samples > 0
        || q.streams.iter().any(|s| {
            s.metrics.recent.starvation_samples > 0
                || s.metrics.recent.pcm_overflow_samples > 0
                || s.metrics.recent.jitter_overflow_packets > 0
        })
    {
        causes.push("Local audio buffering pressure");
    }
    if c.outgoing_full_packets > 0
        || c.outgoing_expired_packets > 0
        || c.outgoing_failed_packets > 0
        || c.incoming_queue_dropped_packets > 0
    {
        causes.push("Local transport drops");
    }
    if c.input_clipped_samples > 0 || c.output_clipped_samples > 0 {
        causes.push("Clipping indicators");
    }
    if q.streams
        .iter()
        .any(|s| s.metrics.recent.late_packets > 0 || s.metrics.recent.sequence_gap_packets > 0)
    {
        causes.push("Late/missing packets; cause uncertain");
    }
    if q.streams
        .iter()
        .any(|s| s.metrics.recent.decode_errors > 0 || s.metrics.recent.invalid_packets > 0)
    {
        causes.push("Media decode errors; inspect advanced details");
    }
    if causes.is_empty() {
        causes.push("No recent measured quality faults");
    }
    format!(
        "{} · SFU-hop RTT {}",
        causes.join(" · "),
        optional(q.sfu_hop_rtt_ms, "ms")
    )
}
fn optional(v: Option<f64>, unit: &str) -> String {
    v.filter(|v| v.is_finite())
        .map(|v| format!("{v:.1} {unit}"))
        .unwrap_or_else(|| "unavailable".into())
}
fn percent(n: u64, d: u64) -> String {
    optional((d > 0).then(|| n as f64 * 100.0 / d as f64), "%")
}
fn rate(n: u64, ms: u64) -> String {
    optional((ms > 0).then(|| n as f64 * 1000.0 / ms as f64), "packets/s")
}
fn timing(out: &mut String, name: &str, t: &QualityTiming, budget: u64) {
    let _ = writeln!(
        out,
        "{name}: {} calls; {} >{budget} ms; p50/p95/p99 {}/{}/{}; max {}",
        t.calls,
        t.deadline_misses,
        optional(t.p50_us.map(|v| v as f64 / 1000.0), "ms"),
        optional(t.p95_us.map(|v| v as f64 / 1000.0), "ms"),
        optional(t.p99_us.map(|v| v as f64 / 1000.0), "ms"),
        optional(t.max_us.map(|v| v as f64 / 1000.0), "ms")
    );
}
fn counts(out: &mut String, m: &QualityMetrics, stream: bool) {
    let r = &m.recent;
    let t = &m.total;
    let _ = writeln!(
        out,
        "Recent {:.1} s / total since {} (counts unless stated):",
        m.window_ms as f64 / 1000.0,
        if stream {
            "source assignment"
        } else {
            "session"
        }
    );
    macro_rules! row {
        ($label:literal,$field:ident) => {
            let _ = writeln!(out, concat!($label, ": {} / {}"), r.$field, t.$field);
        };
    }
    if stream {
        row!("Received packets", received_packets);
        row!("Duplicate packets", duplicate_packets);
        row!("Reordered packets", reordered_packets);
        row!("Late discards (packets)", late_packets);
        row!(
            "Confirmed sequence gaps (packets; cause unknown)",
            sequence_gap_packets
        );
        row!("Jitter full (packets)", jitter_overflow_packets);
        row!(
            "Jitter resync/control discards (packets)",
            resync_discarded_packets
        );
        row!("Invalid payloads (packets)", invalid_packets);
        row!("Decode errors (packets)", decode_errors);
        row!("Decoded mono samples", decoded_samples);
        row!("PCM overflow samples", pcm_overflow_samples);
        row!(
            "Playback starvation samples (buffered media waiting)",
            starvation_samples
        );
        row!(
            "Unclassified empty samples (may be inactivity/DTX)",
            empty_unknown_samples
        );
        row!("Intentional silence samples", intentional_silence_samples);
        row!("PCM policy discard samples", pcm_discarded_samples);
        row!("FEC attempts (packets)", fec_attempts);
        let _ = writeln!(
            out,
            "Receive rate: {}; duplicates: {}; late: {}",
            rate(r.received_packets, m.window_ms),
            percent(r.duplicate_packets, r.received_packets),
            percent(r.late_packets, r.received_packets)
        );
        let _ = writeln!(
            out,
            "PLC duration: {:.1} / {:.1} ms; recent fraction of decoded samples: {}",
            r.plc_samples as f64 / 48.0,
            t.plc_samples as f64 / 48.0,
            percent(r.plc_samples, r.decoded_samples)
        );
        timing(out, "Opus decode", &m.decode, 20);
    } else {
        row!("Capture overflow samples", capture_overflow_samples);
        row!("Reference overflow samples", reference_overflow_samples);
        row!("Input samples", input_samples);
        row!("Input near-full-scale samples", input_clipped_samples);
        row!("Output mono frames", output_samples);
        row!("Output clipped frames", output_clipped_samples);
        row!("Outgoing offered packets", outgoing_offered_packets);
        row!("Outgoing queue-full packets", outgoing_full_packets);
        row!("Outgoing expired packets", outgoing_expired_packets);
        row!(
            "Outgoing control-invalidated packets",
            outgoing_invalidated_packets
        );
        row!("Accepted by WebRTC (packets)", outgoing_sent_packets);
        row!("WebRTC send failures (packets)", outgoing_failed_packets);
        row!(
            "Incoming application queue drops (packets)",
            incoming_queue_dropped_packets
        );
        row!("Loopback PCM overflow samples", pcm_overflow_samples);
        let _ = writeln!(
            out,
            "Recent input/output clipping: {} / {}; send offered rate: {}",
            percent(r.input_clipped_samples, r.input_samples),
            percent(r.output_clipped_samples, r.output_samples),
            rate(r.outgoing_offered_packets, m.window_ms)
        );
        let _ = writeln!(
            out,
            "Outgoing queue full/expiry: {} / {} of offered packets",
            percent(r.outgoing_full_packets, r.outgoing_offered_packets),
            percent(r.outgoing_expired_packets, r.outgoing_offered_packets)
        );
        timing(out, "Capture DSP", &m.capture, 10);
        timing(out, "Reference DSP", &m.render, 10);
        timing(out, "Opus encode", &m.encode, 20);
        timing(out, "Loopback decode", &m.decode, 20);
    }
}
pub fn detail(value: Option<&AudioQuality>) -> String {
    let Some(q) = value else {
        return "Join native voice or start an audio test. Metrics are unavailable in browser preview and older clients.".into();
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Local observations; no audio is recorded. Timing percentiles are histogram upper bounds.\nSFU-hop RTT: {} (not end-to-end latency)",
        optional(q.sfu_hop_rtt_ms, "ms")
    );
    let _ = writeln!(
        out,
        "WebRTC audio receive/loss estimate, SFU → client, cumulative packets: {} / {}",
        q.sfu_hop_received_packets
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unavailable".into()),
        q.sfu_hop_lost_packets
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    let _ = writeln!(
        out,
        "End-to-end loss: {}; end-to-end latency: {}; limiter activity: {}",
        optional(q.end_to_end_loss_percent, "%"),
        optional(q.end_to_end_latency_ms, "ms"),
        optional(q.limiter_reduction_db.map(f64::from), "dB")
    );
    let _ = writeln!(
        out,
        "Capture/reference queues: {} / {} ten-ms blocks (capacity 12 each)",
        q.capture_blocks, q.reference_blocks
    );
    counts(&mut out, &q.metrics, false);
    for s in &q.streams {
        let _ = writeln!(
            out,
            "\nStream slot {} · jitter {}/8 packets · PCM {}/5760 samples · buffered playout {:.1} ms (excludes device/network/DSP delay)",
            s.slot, s.jitter_packets, s.pcm_samples, s.buffered_playout_ms
        );
        counts(&mut out, &s.metrics, true);
        let _ = writeln!(
            out,
            "Verified FEC recoveries: {}",
            s.verified_fec_packets
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unavailable (decoder currently uses PLC only)".into())
        );
    }
    out.push_str("\nEmpty queues and PLC do not prove audible failure or network loss. Stream starvation is not a device xrun. Totals reset on reconnect; source totals reset on slot reassignment. Disk logging remains separately opt-in.\nFor clipping, check microphone/output gain. For processing misses, compare optional DSP settings under the same CPU load. For late packets, compare local queue drops before attributing the issue to the network.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::audio::QualityCounts;
    #[test]
    fn missing_metrics_remain_unavailable_and_cpu_is_not_a_bad_connection() {
        assert!(summary(None).contains("unavailable"));
        let mut q = AudioQuality::default();
        assert!(detail(Some(&q)).contains("End-to-end loss: unavailable"));
        assert!(!detail(Some(&q)).contains("NaN"));
        q.metrics.capture.deadline_misses = 2;
        let text = summary(Some(&q));
        assert!(text.contains("Local processing"));
        assert!(!text.contains("bad connection"));
        q.metrics.recent = QualityCounts {
            outgoing_invalidated_packets: 50,
            ..Default::default()
        };
        q.metrics.capture.deadline_misses = 0;
        assert!(summary(Some(&q)).contains("No recent measured"));
    }
}
