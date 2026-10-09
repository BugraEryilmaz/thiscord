//! Quarantine stream destruction: WASAPI Drop may wait forever for a driver thread.
use super::*;
use std::sync::{OnceLock, atomic::AtomicUsize};

const MAX_DEVICES: usize = 4;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static REAPER: OnceLock<Option<mpsc::SyncSender<Retired>>> = OnceLock::new();
pub(super) struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Retired {
    streams: Box<dyn DeviceStreams>,
    _permit: Permit,
}
fn reaper() -> Option<&'static mpsc::SyncSender<Retired>> {
    REAPER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Retired>(MAX_DEVICES);
        thread::Builder::new().name("thiscord-audio-cleanup".into()).spawn(move || {
            while let Ok(retired) = rx.recv() {
                let start = Instant::now();
                diagnostics::event("stream_cleanup_begin", serde_json::json!({"live_device_sets":live()}));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(retired)));
                diagnostics::event("stream_cleanup_end", serde_json::json!({"elapsed_ms":start.elapsed().as_millis(),"panicked":result.is_err(),"live_device_sets":live()}));
            }
        }).ok().map(|_|tx)
    }).as_ref()
}
pub(super) fn live() -> usize {
    LIVE.load(Ordering::Acquire)
}
pub(super) fn acquire() -> Result<Permit, String> {
    reaper().ok_or("Cannot start audio cleanup worker")?;
    LIVE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_DEVICES).then_some(n + 1))
        .map_err(|_| "Audio driver cleanup is still blocked. Open diagnostic logs, then fully exit and restart Thiscord.")?;
    Ok(Permit)
}

pub(super) struct Managed {
    retired: Option<Retired>,
    health: Arc<StreamHealth>,
}
impl Managed {
    pub fn new(
        streams: impl DeviceStreams + 'static,
        permit: Permit,
        health: Arc<StreamHealth>,
    ) -> Self {
        Self {
            retired: Some(Retired {
                streams: Box::new(streams),
                _permit: permit,
            }),
            health,
        }
    }
}
impl DeviceStreams for Managed {
    fn play(&self) -> Result<(), String> {
        self.retired
            .as_ref()
            .ok_or("Audio stream was retired")?
            .streams
            .play()
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        // Even a hung driver cannot feed fresh capture/playback after retirement.
        self.health.disable();
        if let Some(retired) = self.retired.take() {
            if let Some(sender) = reaper() {
                if let Err(error) = sender.try_send(retired) {
                    // Retain the permit permanently. Never block control or grow
                    // an unbounded pile of driver threads if cleanup has failed.
                    let (mpsc::TrySendError::Full(retired)
                    | mpsc::TrySendError::Disconnected(retired)) = error;
                    std::mem::forget(retired);
                    diagnostics::event(
                        "stream_cleanup_unavailable",
                        serde_json::json!({"live_device_sets":live()}),
                    );
                }
            } else {
                std::mem::forget(retired);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_stream_drop_does_not_block_control_and_disables_callbacks() {
        struct Blocking(mpsc::Receiver<()>, mpsc::Sender<()>);
        impl DeviceStreams for Blocking {
            fn play(&self) -> Result<(), String> {
                Ok(())
            }
        }
        impl Drop for Blocking {
            fn drop(&mut self) {
                let _ = self.1.send(());
                let _ = self.0.recv();
            }
        }
        let (release, wait) = mpsc::channel();
        let (began, started) = mpsc::channel();
        let health = Arc::new(StreamHealth::default());
        let managed = Managed::new(Blocking(wait, began), acquire().unwrap(), health.clone());
        let start = Instant::now();
        drop(managed);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(health.disabled());
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        // Other control work is free to run while cleanup is still waiting.
        let extra = acquire().unwrap();
        let extra2 = acquire().unwrap();
        let extra3 = acquire().unwrap();
        assert!(acquire().is_err());
        drop((extra, extra2, extra3));
        release.send(()).unwrap();
    }
}
