//! Audio callbacks record fixed-size failure codes; formatting stays on the worker.
use cpal::ErrorKind;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Default)]
pub(super) struct StreamHealth {
    failure: AtomicU8,
}

impl StreamHealth {
    pub(super) fn report(&self, input: bool, kind: ErrorKind) {
        let code = match kind {
            // CPAL reports these while the stream remains usable. Neither means
            // a device was unplugged or its stream needs rebuilding.
            ErrorKind::Xrun | ErrorKind::RealtimeDenied => return,
            // Preserve explicit rejoin on route changes: a microphone must not
            // silently switch to a different device, even if CPAL reroutes it.
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
        Err(format!(
            "{device}: {reason}. Select devices and start again."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn device_changes_and_invalid_streams_require_explicit_restart() {
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
