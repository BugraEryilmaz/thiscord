//! Audio callbacks record fixed-size failure codes; formatting stays on the worker.
use cpal::ErrorKind;
use std::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct StreamHealth {
    failure: AtomicU8,
    disabled: AtomicBool,
    input: AtomicU64,
    output: AtomicU64,
    glitches: AtomicU64,
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
    pub fn counters(&self) -> [u64; 2] {
        [
            self.input.load(Ordering::Relaxed),
            self.output.load(Ordering::Relaxed),
        ]
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"callbacks":self.counters(),"failure_code":self.failure.load(Ordering::Acquire),"glitches":self.glitches.load(Ordering::Relaxed),"disabled":self.disabled()})
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
}
impl Default for Watch {
    fn default() -> Self {
        Self {
            counts: [0; 2],
            changed: [Instant::now(); 2],
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
