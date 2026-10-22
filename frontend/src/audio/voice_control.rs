//! One outstanding audio-worker request per call. Poll its completion alongside
//! signaling so slow device/DSP work cannot hold up privacy acknowledgments.
use super::connection::Connection;
use std::sync::Mutex;
use thiscord_shared::audio::{AudioSettings, AudioStatus};

/// A cancelled blocking startup can outlive the join/leave task. Check its
/// lease under the same settings lock used by replacement calls, before publish.
pub fn publish_start_settings(
    connection: &Connection,
    target: &Mutex<AudioSettings>,
    settings: &AudioSettings,
) -> Result<(), String> {
    let mut current = target.lock().map_err(|_| "Voice settings unavailable")?;
    if !connection.active() {
        return Err("Voice connection was cancelled".into());
    }
    *current = settings.clone();
    Ok(())
}

#[derive(Default)]
pub struct VoiceControl {
    task: Option<tokio::task::JoinHandle<Result<AudioStatus, String>>>,
}

impl VoiceControl {
    pub fn pending(&self) -> bool {
        self.task.is_some()
    }

    pub fn start(&mut self, work: impl FnOnce() -> Result<AudioStatus, String> + Send + 'static) {
        // Do not replace/resubmit a queued worker command on every tick.
        if self.task.is_none() {
            self.task = Some(tokio::task::spawn_blocking(work));
        }
    }

    pub async fn completed(&mut self) -> Result<AudioStatus, String> {
        let Some(task) = &mut self.task else {
            return std::future::pending().await;
        };
        let result = task.await;
        self.task = None;
        result.map_err(|_| "Audio task failed".to_owned())?
    }
}

impl Drop for VoiceControl {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            // A running blocking command cannot be aborted. The owning call's
            // Connection is closed separately, fencing its late completion.
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::red::boundary::Boundary;
    use std::{sync::mpsc, time::Instant};

    #[test]
    fn cancelled_start_waiting_for_settings_cannot_overwrite_replacement() {
        let settings = std::sync::Arc::new(Mutex::new(AudioSettings::default()));
        let mut locked = settings.lock().unwrap();
        let connection = Connection::default();
        let old = connection.clone();
        let target = settings.clone();
        let (entered, waiting) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            publish_start_settings(&old, &target, &AudioSettings::default())
        });
        waiting.recv().unwrap();
        connection.close();
        locked.muted = true;
        locked.deafened = true;
        drop(locked);
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            "Voice connection was cancelled"
        );
        let current = settings.lock().unwrap();
        assert!(current.muted);
        assert!(current.deafened);
    }

    #[tokio::test]
    async fn stalled_audio_request_does_not_delay_privacy_ack_or_duplicate_work() {
        let (release, held) = mpsc::channel();
        let mut control = VoiceControl::default();
        control.start(move || {
            held.recv().unwrap();
            Err("device failed".into())
        });
        let mut boundary = Boundary::default();
        boundary.control(1).unwrap();
        boundary.request(Instant::now()).unwrap();
        assert!(boundary.acknowledge(1, None, true));
        boundary.require_refresh(7);
        boundary.start_refresh(2).unwrap();
        boundary.request(Instant::now()).unwrap();
        let (signal, mut signals) = tokio::sync::mpsc::channel(1);
        signal.send((2, Some(7), true)).await.unwrap();
        // This is the call loop's selection: a blocked audio command must not
        // prevent processing the ACK and sending Ready before server expiry.
        tokio::select! { biased;
            _ = control.completed() => panic!("audio is still blocked"),
            ack = signals.recv() => {
                let (generation, challenge, accepted) = ack.unwrap();
                assert!(boundary.acknowledge(generation, challenge, accepted));
                assert_eq!(boundary.ready_notice(), Some((2, 7)));
            }
        }
        assert!(control.pending(), "cancelled selection retains the worker");
        control.start(|| panic!("a second worker must not be submitted"));
        release.send(()).unwrap();
        assert_eq!(control.completed().await.unwrap_err(), "device failed");
        assert!(!control.pending());
        control.start(|| Err("next request".into()));
        assert_eq!(control.completed().await.unwrap_err(), "next request");
    }

    #[tokio::test]
    async fn leaving_during_startup_fences_late_worker_command_and_completion() {
        use crate::audio::{AudioEngine, Command, connection::Connection, volumes};
        use thiscord_shared::audio::AudioSettings;

        let engine = AudioEngine::new();
        engine.restrict(&AudioSettings {
            muted: true,
            ..Default::default()
        });
        let connection = Connection::default();
        let old = connection.clone();
        let worker = engine.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, held) = mpsc::channel();
        let (finished, late_result) = tokio::sync::oneshot::channel();
        let mut control = VoiceControl::default();
        control.start(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
            let result = worker.command(Command::VoiceStart {
                volumes: volumes::Store::default()
                    .profile("00000000-0000-0000-0000-000000000001".parse().unwrap()),
                connection: old,
                settings: AudioSettings::default(),
                outgoing: tokio::sync::mpsc::channel(1).0,
                microphone: true,
            });
            finished.send(result.is_err()).unwrap();
            result
        });
        started.await.unwrap();
        // Leave and access revocation both close this call's lease. A blocking
        // request already running can finish after cancellation/new-call startup.
        connection.close();
        drop(control);
        let mut replacement = VoiceControl::default();
        replacement.start(|| Err("replacement status".into()));
        release.send(()).unwrap();
        assert!(late_result.await.unwrap());
        assert_eq!(
            replacement.completed().await.unwrap_err(),
            "replacement status"
        );
        assert!(engine.inhibit.load(std::sync::atomic::Ordering::Acquire));
        assert!(!engine.command(Command::Peek).unwrap().running);
        assert!(engine.current_settings().unwrap().is_none());
    }
}
