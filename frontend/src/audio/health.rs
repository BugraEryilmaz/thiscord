//! Audio callbacks record fixed-size failure codes; formatting stays on the worker.
use cpal::ErrorKind;
use std::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Capture is compared with wall time over windows of at least this length.
const RATE_WINDOW: Duration = Duration::from_secs(1);
/// A healthy device delivers about 1x real time. Allow catch-up after a stall
/// (bounded by the device buffer) before declaring the clock broken.
const FAST_FACTOR: f64 = 1.5;
const FAST_SLACK: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct StreamHealth {
    failure: AtomicU8,
    disabled: AtomicBool,
    input: AtomicU64,
    output: AtomicU64,
    glitches: AtomicU64,
    /// Device frames captured, at `input_rate` Hz (0 before an input opens).
    input_frames: AtomicU64,
    input_rate: AtomicU32,
}

impl StreamHealth {
    pub fn disable(&self) {
        self.disabled.store(true, Ordering::Release);
    }
    pub fn disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }
    pub fn callback(&self, input: bool) {
        if input { &self.input } else { &self.output }.fetch_add(1, Ordering::Relaxed);
    }
    /// Set once per stream before capture starts.
    pub fn input_rate(&self, rate: u32) {
        self.input_rate.store(rate, Ordering::Relaxed);
    }
    /// One relaxed add per capture callback; no locks or allocations.
    pub fn captured(&self, frames: usize) {
        self.input_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
    pub fn counters(&self) -> [u64; 2] {
        [
            self.input.load(Ordering::Relaxed),
            self.output.load(Ordering::Relaxed),
        ]
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"callbacks":self.counters(),"input_frames":self.input_frames.load(Ordering::Relaxed),"input_rate":self.input_rate.load(Ordering::Relaxed),"failure_code":self.failure.load(Ordering::Acquire),"glitches":self.glitches.load(Ordering::Relaxed),"disabled":self.disabled()})
    }
    pub(super) fn report(&self, input: bool, kind: ErrorKind) {
        let code = match kind {
            // CPAL reports these while the stream remains usable. Neither means
            // a device was unplugged or its stream needs rebuilding.
            ErrorKind::Xrun | ErrorKind::RealtimeDenied => {
                self.glitches.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // Reopen through bounded recovery rather than accepting an implicit
            // backend reroute without updating the recovery status.
            ErrorKind::DeviceChanged => 1,
            ErrorKind::DeviceNotAvailable => 2,
            ErrorKind::StreamInvalidated => 3,
            ErrorKind::PermissionDenied => 4,
            ErrorKind::DeviceBusy => 5,
            ErrorKind::HostUnavailable => 6,
            ErrorKind::UnsupportedConfig => 7,
            ErrorKind::ResourceExhausted => 8,
            ErrorKind::BackendError => 9,
            ErrorKind::InvalidInput => 10,
            ErrorKind::UnsupportedOperation => 11,
            _ => 12,
        };
        // Keep the first failure, including its direction, even if notifications
        // race or follow it during stream teardown. No locks or allocations.
        let _ = self.failure.compare_exchange(
            0,
            code | if input { 0x80 } else { 0 },
            Ordering::Release,
            Ordering::Relaxed,
        );
    }

    pub(super) fn check(&self) -> Result<(), String> {
        let failure = self.failure.load(Ordering::Acquire);
        if failure == 0 {
            return Ok(());
        }
        let device = if failure & 0x80 != 0 {
            "Microphone"
        } else {
            "Output"
        };
        let reason = match failure & 0x7f {
            1 => "audio route changed (DeviceChanged)",
            2 => "device is unavailable or disconnected (DeviceNotAvailable)",
            3 => "audio format or stream changed (StreamInvalidated)",
            4 => "device access was denied; check OS audio permissions (PermissionDenied)",
            5 => "device is busy (DeviceBusy)",
            6 => "audio subsystem is unavailable (HostUnavailable)",
            7 => "audio format is unsupported (UnsupportedConfig)",
            8 => "OS audio resources were exhausted (ResourceExhausted)",
            9 => "audio driver reported an error (BackendError)",
            10 => "audio backend rejected a parameter (InvalidInput)",
            11 => "audio operation is unsupported (UnsupportedOperation)",
            _ => "audio stream failed (Other)",
        };
        Err(format!("{device}: {reason}."))
    }
}

pub(super) struct Watch {
    counts: [u64; 2],
    changed: [Instant; 2],
    /// Start of the current capture-rate window and the frame count then.
    window: Option<(Instant, u64)>,
}
impl Default for Watch {
    fn default() -> Self {
        Self {
            counts: [0; 2],
            changed: [Instant::now(); 2],
            window: None,
        }
    }
}
impl Watch {
    pub fn check(
        &mut self,
        health: &StreamHealth,
        microphone: bool,
        now: Instant,
    ) -> Result<(), String> {
        for (index, count) in health.counters().into_iter().enumerate() {
            if self.counts[index] != count {
                self.counts[index] = count;
                self.changed[index] = now;
            }
            if (index == 1 || microphone)
                && now.duration_since(self.changed[index]) >= Duration::from_secs(5)
            {
                return Err(format!(
                    "{} callbacks stopped for five seconds; the audio driver may be stalled.",
                    if index == 0 { "Microphone" } else { "Output" }
                ));
            }
        }
        if microphone {
            self.check_rate(health, now)?;
        }
        Ok(())
    }

    /// A capture clock running fast (seen with a stuck Windows audio engine at
    /// about 4.6x) would encode and send that many packets per second, which the
    /// server refuses. Report it as a device failure so recovery reopens it.
    fn check_rate(&mut self, health: &StreamHealth, now: Instant) -> Result<(), String> {
        let rate = health.input_rate.load(Ordering::Relaxed);
        let frames = health.input_frames.load(Ordering::Relaxed);
        let Some((start, base)) = self.window else {
            self.window = Some((now, frames));
            return Ok(());
        };
        let elapsed = now.saturating_duration_since(start);
        if rate == 0 || elapsed < RATE_WINDOW {
            return Ok(());
        }
        self.window = Some((now, frames));
        let delivered = frames.saturating_sub(base) as f64 / f64::from(rate);
        let allowed = elapsed.as_secs_f64() * FAST_FACTOR + FAST_SLACK.as_secs_f64();
        if delivered > allowed {
            return Err(format!(
                "Microphone delivered audio {:.1}x faster than real time; the audio driver may be malfunctioning.",
                delivered / elapsed.as_secs_f64()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalled_capture_is_detected_even_while_output_is_alive() {
        let health = StreamHealth::default();
        let mut watch = Watch::default();
        let now = Instant::now() + Duration::from_secs(6);
        health.callback(false);
        assert!(
            watch
                .check(&health, true, now)
                .unwrap_err()
                .contains("Microphone")
        );
        assert!(watch.check(&health, false, now).is_ok());
        health.callback(true);
        assert!(watch.check(&health, true, now).is_ok());
    }

    #[test]
    fn capture_faster_than_real_time_is_a_microphone_failure() {
        let health = StreamHealth::default();
        health.input_rate(48_000);
        let mut watch = Watch::default();
        let start = Instant::now();
        let tick = |watch: &mut Watch, at: Duration| {
            // Keep callbacks alive so only the rate check can fail.
            health.callback(true);
            health.callback(false);
            watch.check(&health, true, start + at)
        };
        // Real time, then a catch-up burst after a short stall.
        assert!(tick(&mut watch, Duration::ZERO).is_ok());
        for second in 1..=3 {
            health.captured(48_000);
            assert!(tick(&mut watch, Duration::from_secs(second)).is_ok());
        }
        health.captured(48_000 + 24_000);
        assert!(tick(&mut watch, Duration::from_secs(4)).is_ok());
        // The observed failure: about 4.6x real time.
        health.captured(4 * 48_000 + 28_800);
        let error = tick(&mut watch, Duration::from_secs(5)).unwrap_err();
        assert!(error.starts_with("Microphone delivered audio 4.6x faster"));
    }

    #[test]
    fn capture_rate_is_not_judged_early_or_without_a_microphone() {
        let health = StreamHealth::default();
        health.input_rate(48_000);
        let mut watch = Watch::default();
        let start = Instant::now();
        health.callback(true);
        health.callback(false);
        assert!(watch.check(&health, true, start).is_ok());
        health.captured(10 * 48_000);
        // Less than one window: a device's initial buffer is not judged.
        assert!(
            watch
                .check(&health, true, start + Duration::from_millis(900))
                .is_ok()
        );
        // Listener sessions have no capture stream to judge.
        assert!(
            watch
                .check(&health, false, start + Duration::from_secs(2))
                .is_ok()
        );
    }

    #[test]
    fn buffer_glitches_and_scheduling_warnings_do_not_stop_audio() {
        let health = StreamHealth::default();
        for input in [true, false] {
            for _ in 0..100 {
                health.report(input, ErrorKind::Xrun);
                health.report(input, ErrorKind::RealtimeDenied);
            }
        }
        assert!(health.check().is_ok());
        health.report(true, ErrorKind::DeviceNotAvailable);
        assert!(
            health
                .check()
                .unwrap_err()
                .contains("Microphone: device is unavailable")
        );
    }

    #[test]
    fn device_changes_and_invalid_streams_report_direction_for_recovery() {
        for kind in [
            ErrorKind::DeviceChanged,
            ErrorKind::StreamInvalidated,
            ErrorKind::PermissionDenied,
        ] {
            for input in [true, false] {
                let health = StreamHealth::default();
                health.report(input, kind);
                let error = health.check().unwrap_err();
                assert!(error.starts_with(if input { "Microphone:" } else { "Output:" }));
                assert!(error.contains(&format!("{kind:?}")));
            }
        }
    }

    #[test]
    fn later_notifications_do_not_hide_the_original_failure() {
        let health = StreamHealth::default();
        health.report(false, ErrorKind::BackendError);
        health.report(true, ErrorKind::DeviceNotAvailable);
        health.report(false, ErrorKind::Xrun);
        let error = health.check().unwrap_err();
        assert!(error.starts_with("Output:"));
        assert!(error.contains("BackendError"));
        assert!(!error.contains("disconnected"));
        // Restarting creates new health state; a previous stream cannot poison it.
        assert!(StreamHealth::default().check().is_ok());
    }
}
