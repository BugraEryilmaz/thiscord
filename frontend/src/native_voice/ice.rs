//! Desktop side of in-place ICE restarts (`thiscord_frontend::ice_recovery`):
//! the voice loop's clock and the selected route's candidate type.
pub(crate) use thiscord_frontend::ice_recovery::{Action, IceRecovery};

/// Milliseconds on the call's monotonic clock.
pub(crate) fn millis(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether the nominated pair's local candidate is a TURN relay.
pub(crate) fn relayed(report: &rtc::statistics::report::RTCStatsReport) -> Option<bool> {
    use rtc::statistics::report::RTCStatsReportEntry as Entry;
    let pair = report.iter().find_map(|entry| match entry {
        Entry::IceCandidatePair(pair) if pair.nominated => Some(pair),
        _ => None,
    })?;
    report.iter().find_map(|entry| match entry {
        Entry::LocalCandidate(candidate) if candidate.stats.id == pair.local_candidate_id => {
            Some(candidate.candidate_type.to_string() == "relay")
        }
        _ => None,
    })
}
