use super::*;
use rtc::statistics::report::{RTCStatsReport, RTCStatsReportEntry as Entry};
use thiscord_shared::admin::{NetworkMetrics, ParticipantDiagnostics};

pub(super) struct Sampler(pub tokio::task::JoinHandle<()>);
impl Drop for Sampler {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn network(report: &RTCStatsReport) -> NetworkMetrics {
    let mut value = NetworkMetrics {
        sampled_at: Some(chrono::Utc::now()),
        ..Default::default()
    };
    for entry in report.iter() {
        match entry {
            Entry::IceCandidatePair(s) if s.nominated && s.responses_received > 0 => {
                value.round_trip_ms = milliseconds(s.current_round_trip_time);
            }
            Entry::InboundRtp(s) => {
                let s = &s.received_rtp_stream_stats;
                if s.rtp_stream_stats.ssrc == MediaKind::Microphone.publisher_ssrc()
                    && s.packets_received > 0
                {
                    // rtc 0.21's interceptor copies the raw RTCP report.jitter
                    // into stats, despite documenting seconds. Opus uses 48 kHz.
                    value.microphone_jitter_ms = microphone_jitter_ms(s.jitter);
                    value.microphone_packets_received = Some(s.packets_received);
                    value.microphone_packets_lost = Some(s.packets_lost);
                }
            }
            _ => {}
        }
    }
    value
}
fn milliseconds(seconds: f64) -> Option<f64> {
    let ms = seconds * 1000.0;
    (ms.is_finite() && ms >= 0.0).then_some(ms)
}
fn microphone_jitter_ms(ticks: f64) -> Option<f64> {
    milliseconds(ticks / 48_000.0)
}
pub(super) async fn snapshot() -> HashMap<ChannelId, Vec<ParticipantDiagnostics>> {
    // Never hold the registry or membership locks while waiting for telemetry.
    let rooms: Vec<_> = ROOMS
        .get_or_init(Default::default)
        .lock()
        .await
        .iter()
        .filter_map(|(id, r)| Some((*id, r.upgrade()?)))
        .collect();
    let mut result = HashMap::new();
    for (id, room) in rooms {
        let members: Vec<_> = room.members.read().await.values().cloned().collect();
        let mut participants = Vec::new();
        for member in members {
            if !member.active.load(Ordering::Acquire) {
                continue;
            }
            let network = member.network.read().await.clone();
            participants.push(ParticipantDiagnostics {
                participant: member.info,
                network,
                video_counters: std::array::from_fn(|i| member.metrics[i].load(Ordering::Relaxed)),
            });
        }
        participants.sort_by(|a, b| a.participant.username.cmp(&b.participant.username));
        if !participants.is_empty() {
            result.insert(id, participants);
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_is_not_zero_and_units_are_milliseconds() {
        assert_eq!(milliseconds(0.025), Some(25.0));
        assert_eq!(milliseconds(f64::NAN), None);
        assert_eq!(milliseconds(-1.0), None);
        assert_eq!(microphone_jitter_ms(480.0), Some(10.0));
        assert_eq!(NetworkMetrics::default().round_trip_ms, None);
    }
}
