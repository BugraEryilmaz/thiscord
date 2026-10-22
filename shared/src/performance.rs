//! Numeric, content-free diagnostics. Counters are cumulative and reset on restart.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Timing {
    pub calls: u64,
    pub total_us: u64,
    pub max_us: u64,
    pub over_budget: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub name: String,
    pub budget_us: u64,
    pub timing: Timing,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BackendPerformance {
    pub timings: Vec<Measurement>,
    pub pool_failures: u64,
    pub roster_unchanged: u64,
    pub friends_unchanged: u64,
    pub friends_admission_skipped: u64,
}

/// Fixed slots prevent arbitrary labels, content or unbounded telemetry storage.
pub const UI_TIMINGS: [(&str, u64); 4] = [
    ("Friend row snapshot lookup", 1_000),
    ("Chat batch merge", 16_000),
    ("Chat layout / viewport", 16_000),
    ("Visible UI timer lateness", 50_000),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UiPerformance {
    pub timings: [Timing; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiPerformanceReport {
    pub age_ms: u64,
    pub measurements: UiPerformance,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_payload_has_fixed_numeric_slots() {
        let value = serde_json::to_value(UiPerformance::default()).unwrap();
        assert_eq!(value["timings"].as_array().unwrap().len(), UI_TIMINGS.len());
        let mut oversized = value;
        oversized["timings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({}));
        assert!(serde_json::from_value::<UiPerformance>(oversized).is_err());
    }

    #[test]
    fn legacy_diagnostics_and_audio_status_remain_readable() {
        let legacy = serde_json::json!({
            "sampled_at":"2026-10-22T10:00:00Z", "role":"owner", "version":"old",
            "uptime_seconds":5, "host":{}, "database_connections":1,
            "database_idle_connections":1, "rooms":[]
        });
        let diagnostics: crate::admin::Diagnostics = serde_json::from_value(legacy).unwrap();
        assert!(diagnostics.performance.timings.is_empty());
        let legacy = serde_json::json!({
            "running":true, "input_level":0.0, "transmitting":false,
            "message":"", "streams":[], "dropped_samples":0, "underrun_samples":0
        });
        let mut status: crate::audio::AudioStatus = serde_json::from_value(legacy).unwrap();
        assert!(status.ui_performance.is_none());
        let mut measurements = UiPerformance::default();
        measurements.timings[0].calls = 7;
        status.ui_performance = Some(UiPerformanceReport {
            age_ms: 10,
            measurements,
        });
        let round_trip: crate::audio::AudioStatus =
            serde_json::from_value(serde_json::to_value(status).unwrap()).unwrap();
        let report = round_trip.ui_performance.unwrap();
        assert_eq!(report.age_ms, 10);
        assert_eq!(report.measurements.timings[0].calls, 7);
    }
}
