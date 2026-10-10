//! Per-call retry budget. Device opening and diagnostics never block the media worker.
use super::*;

const ATTEMPTS: usize = 3;
const DELAY: Duration = Duration::from_millis(500);
const DEADLINE: Duration = Duration::from_secs(8);
// A hung OS call cannot be killed safely. Retain its slot across session changes.
static OPENING: AtomicBool = AtomicBool::new(false);

enum ResultKind {
    Device(Result<Devices, String>),
    Diagnosis(String),
}
struct Job {
    result: mpsc::Receiver<ResultKind>,
    started: Instant,
}
#[derive(Default)]
pub(super) struct Recovery {
    initial: bool,
    attempts: usize,
    active: bool,
    next: Option<Instant>,
    job: Option<Job>,
    error: String,
    notice: Option<String>,
}
impl Recovery {
    pub fn starting() -> Self {
        Self {
            initial: true,
            active: true,
            next: Some(Instant::now()),
            notice: Some("Opening audio devices…".into()),
            ..Default::default()
        }
    }
    pub fn wait_expired(&self) -> bool {
        self.next
            .is_some_and(|next| Instant::now().saturating_duration_since(next) >= DEADLINE)
    }
    pub fn reconfigured(&mut self, info: &serde_json::Value) {
        self.notice = selection::fallback_notice(info);
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn message(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn begin(&mut self, error: String) {
        diagnostics::event(
            "recovery_needed",
            serde_json::json!({"error":error,"attempts_used":self.attempts}),
        );
        self.initial = false;
        self.error = error;
        self.active = true;
        self.next = Some(Instant::now() + DELAY);
        self.notice = Some(format!(
            "Audio interrupted; recovering devices. {}",
            self.error
        ));
    }
    pub fn succeeded(&mut self, info: &serde_json::Value) {
        let fallback = selection::fallback_notice(info);
        diagnostics::event(
            "recovery_succeeded",
            serde_json::json!({"initial":self.initial,"attempts_used":self.attempts,"defaults":self.attempts>ATTEMPTS,"device":info}),
        );
        self.active = false;
        if self.initial {
            self.initial = false;
            self.attempts = 0; // Normal first open does not spend the recovery budget.
            self.notice = fallback;
            return;
        }
        self.notice = Some(fallback.unwrap_or_else(|| if self.attempts > ATTEMPTS {
            format!(
                "Audio recovered using system defaults. Microphone: {}; output: {}. Saved device preferences unchanged.",
                info["input"]["name"].as_str().unwrap_or("not active"),
                info["output"]["name"].as_str().unwrap_or("default")
            )
        } else {
            "Audio recovered using selected devices".into()
        }));
    }
    fn configuration(&mut self, selected: &AudioSettings) -> Option<AudioSettings> {
        if self.attempts >= 2 * ATTEMPTS {
            return None;
        }
        let mut settings = selected.clone();
        if self.attempts >= ATTEMPTS {
            settings.input = None;
            settings.output = None;
        }
        self.attempts += 1;
        Some(settings)
    }
    fn exhausted(&self, diagnosis: &str) -> String {
        format!(
            "Audio recovery failed after {ATTEMPTS} selected-device and {ATTEMPTS} default-device attempts. {} {diagnosis} Select devices and join again.",
            self.error
        )
    }
    pub fn poll(
        &mut self,
        settings: &AudioSettings,
        microphone: bool,
    ) -> Result<Option<Devices>, String> {
        if let Some(job) = &self.job {
            if job.started.elapsed() >= DEADLINE {
                diagnostics::event(
                    "device_open_timeout",
                    serde_json::json!({"attempts_used":self.attempts,"elapsed_ms":job.started.elapsed().as_millis()}),
                );
                return Err(format!(
                    "Audio recovery timed out waiting for the audio driver. {} Process information is unavailable. Select devices and join again.",
                    self.error
                ));
            }
            let result = match job.result.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("Audio recovery worker stopped. Join again.".into());
                }
            };
            self.job = None;
            match result {
                ResultKind::Device(Ok(devices)) => return Ok(Some(devices)),
                ResultKind::Device(Err(error)) => self.begin(error),
                ResultKind::Diagnosis(diagnosis) => {
                    diagnostics::event(
                        "device_recovery_exhausted",
                        serde_json::json!({"diagnosis":diagnosis,"last_error":self.error}),
                    );
                    return Err(self.exhausted(&diagnosis));
                }
            }
        }
        if self.next.is_some_and(|next| Instant::now() < next) {
            return Ok(None);
        }
        if OPENING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            if self.wait_expired() {
                return Err("An earlier audio driver operation is still blocked. Process information is unavailable. Restart the app after checking the audio device.".into());
            }
            return Ok(None);
        }
        let configuration = self.configuration(settings);
        diagnostics::event(
            "recovery_attempt",
            serde_json::json!({"attempt":self.attempts,"defaults":self.attempts>ATTEMPTS,"diagnosing":configuration.is_none()}),
        );
        let selected = settings.clone();
        self.notice = Some(match &configuration {
            Some(_) => format!(
                "Recovering audio: {} devices, attempt {}/{}. {}",
                if self.attempts > ATTEMPTS {
                    "system default"
                } else {
                    "selected"
                },
                (self.attempts - 1) % ATTEMPTS + 1,
                ATTEMPTS,
                self.error
            ),
            None => "Audio recovery exhausted; checking audio-session processes…".into(),
        });
        let (tx, result) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name("thiscord-audio-recovery".into())
            .spawn(move || {
                struct Release;
                impl Drop for Release {
                    fn drop(&mut self) {
                        OPENING.store(false, Ordering::Release);
                    }
                }
                let _release = Release;
                let result = match configuration {
                    Some(settings) => ResultKind::Device(Devices::prepare(&settings, microphone)),
                    None => ResultKind::Diagnosis(device_users::describe(&selected, microphone)),
                };
                // A cancelled session drops its receiver; paused streams are destroyed here.
                let _ = tx.send(result);
            });
        if spawn.is_err() {
            OPENING.store(false, Ordering::Release);
            return Err("Cannot start audio recovery worker".into());
        }
        self.job = Some(Job {
            result,
            started: Instant::now(),
        });
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_open_announces_missing_device_fallback_without_spending_retry_budget() {
        for (input, output, missing) in [
            (false, true, "output is"),
            (true, false, "microphone is"),
            (true, true, "microphone and output are"),
        ] {
            let info = serde_json::json!({
                "input": {"name": "Working microphone", "fallback": input},
                "output": {"name": "Working speakers", "fallback": output}
            });
            let selected = AudioSettings {
                input: Some("saved mic".into()),
                output: Some("saved output".into()),
                ..Default::default()
            };
            let mut recovery = Recovery::starting();
            recovery.configuration(&selected).unwrap();
            recovery.succeeded(&info);
            assert!(!recovery.active());
            assert_eq!(recovery.attempts, 0);
            let notice = recovery.message().unwrap();
            assert!(notice.contains(missing));
            assert!(notice.contains("Working microphone"));
            assert!(notice.contains("Working speakers"));
            assert!(notice.contains("Saved device preferences unchanged"));

            // Unplugging during the call uses the same resolution and notice,
            // but still consumes the bounded recovery budget.
            recovery.begin("device disconnected".into());
            recovery.configuration(&selected).unwrap();
            recovery.succeeded(&info);
            assert!(!recovery.active());
            assert_eq!(recovery.attempts, 1);
            assert!(recovery.message().unwrap().contains(missing));
        }
    }

    #[test]
    fn listener_fallback_does_not_claim_to_open_a_microphone() {
        let mut recovery = Recovery::starting();
        recovery.succeeded(&serde_json::json!({
            "input": null,
            "output": {"name": "Speakers", "fallback": true}
        }));
        let notice = recovery.message().unwrap();
        assert!(notice.contains("Microphone: not active"));
        assert!(notice.contains("output is unavailable"));
        recovery.reconfigured(&serde_json::json!({
            "input": null,
            "output": {"name": "Headset", "fallback": false}
        }));
        assert!(recovery.message().is_none());
    }

    #[test]
    fn selected_then_default_have_equal_finite_budgets() {
        let selected = AudioSettings {
            input: Some("mic".into()),
            output: Some("speaker".into()),
            muted: true,
            ..Default::default()
        };
        let mut recovery = Recovery::default();
        for attempt in 0..6 {
            let settings = recovery.configuration(&selected).unwrap();
            assert!(settings.muted);
            assert_eq!(settings.input.as_deref(), (attempt < 3).then_some("mic"));
            assert_eq!(
                settings.output.as_deref(),
                (attempt < 3).then_some("speaker")
            );
            // A briefly successful stream must not reset the budget.
            recovery.succeeded(&serde_json::Value::Null);
            recovery.begin("busy".into());
        }
        assert!(recovery.configuration(&selected).is_none());
        assert_eq!(selected.input.as_deref(), Some("mic"));
    }
    #[test]
    fn late_preparation_is_discarded_after_cancellation() {
        struct Tracked(Arc<AtomicBool>);
        impl DeviceStreams for Tracked {
            fn play(&self) -> Result<(), String> {
                panic!("cancelled stream must not start")
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let mut devices = Devices::silent(&AudioSettings::default());
        devices.streams = Box::new(Tracked(dropped.clone()));
        let (tx, rx) = mpsc::channel();
        let recovery = Recovery {
            job: Some(Job {
                result: rx,
                started: Instant::now(),
            }),
            ..Default::default()
        };
        drop(recovery);
        drop(tx.send(ResultKind::Device(Ok(devices))));
        assert!(dropped.load(Ordering::Acquire));
    }
    #[test]
    fn pending_driver_timeout_and_terminal_diagnosis_are_bounded() {
        let (tx, rx) = mpsc::channel();
        let mut recovery = Recovery {
            job: Some(Job {
                result: rx,
                started: Instant::now() - DEADLINE,
            }),
            ..Default::default()
        };
        assert!(
            recovery
                .poll(&AudioSettings::default(), true)
                .err()
                .unwrap()
                .contains("timed out")
        );
        drop(tx);
        let (tx, rx) = mpsc::channel();
        recovery.job = Some(Job {
            result: rx,
            started: Instant::now(),
        });
        recovery.attempts = 6;
        tx.send(ResultKind::Diagnosis(
            "Possible conflict: example.exe (PID 123)".into(),
        ))
        .ok()
        .unwrap();
        let error = recovery
            .poll(&AudioSettings::default(), true)
            .err()
            .unwrap();
        assert!(error.contains("3 selected-device and 3 default-device"));
        assert!(error.contains("example.exe (PID 123)"));
    }
    #[test]
    fn first_success_preserves_retry_budget_but_failed_initial_open_does_not_reset_it() {
        let settings = AudioSettings::default();
        let mut recovery = Recovery::starting();
        recovery.configuration(&settings).unwrap();
        recovery.succeeded(&serde_json::Value::Null);
        assert_eq!(recovery.attempts, 0);
        assert!(recovery.message().is_none());
        let mut recovery = Recovery::starting();
        recovery.configuration(&settings).unwrap();
        recovery.begin("busy".into());
        recovery.succeeded(&serde_json::Value::Null);
        assert_eq!(recovery.attempts, 1);
    }
}
