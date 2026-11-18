//! Control/worker-thread diagnostics for one attempt to open a device set.
use super::*;
use serde_json::{Value, json};
use std::sync::atomic::AtomicU64;

#[derive(Clone, Copy)]
pub(super) struct OpenTrace {
    pub id: u64,
    caller: &'static str,
    attempt: Option<usize>,
    join_id: Option<u64>,
    started: Instant,
    microphone: bool,
    requested: [&'static str; 2],
    pub effective: [Option<&'static str>; 2],
}

impl OpenTrace {
    pub fn begin(
        settings: &AudioSettings,
        microphone: bool,
        caller: &'static str,
        attempt: Option<usize>,
        join_id: Option<u64>,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let trace = Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            caller,
            attempt,
            join_id,
            started: Instant::now(),
            microphone,
            requested: [settings.output.is_some(), settings.input.is_some()]
                .map(|saved| if saved { "saved_id" } else { "system_default" }),
            effective: [None; 2],
        };
        trace.event("device_open_begin", || json!({"default_output":settings.output.is_none(),"default_input":settings.input.is_none()}));
        trace
    }
    pub fn selected(&mut self, input: bool, fallback: bool) {
        let index = usize::from(input);
        self.effective[index] = Some(if fallback {
            "fallback_to_default"
        } else if self.requested[index] == "saved_id" {
            "by_id"
        } else {
            "system_default"
        });
    }
    pub fn event(&self, name: &'static str, detail: impl FnOnce() -> Value) {
        diagnostics::event_with(name, || {
            let mut v = detail();
            v["open_id"] = self.id.into();
            v["join_id"] = json!(self.join_id);
            v["caller"] = self.caller.into();
            v["attempt"] = json!(self.attempt);
            v["microphone"] = self.microphone.into();
            if v.get("elapsed_ms").is_none() {
                v["elapsed_ms"] = json!(self.started.elapsed().as_millis());
            }
            for (i, direction) in ["output", "input"].iter().enumerate() {
                v[*direction] =
                    json!({"requested":self.requested[i],"effective":self.effective[i]});
            }
            v
        })
    }
    pub fn fail(
        &self,
        direction: &str,
        stage: &str,
        message: String,
        queries: Option<&format::Queries>,
    ) -> String {
        #[cfg(windows)]
        let at = Instant::now();
        let message = format!("{direction} {stage}: {message}");
        self.event("device_open_failed", || {
            let thread = if stage == "play" {
                thread_identity()
            } else {
                thread_context()
            };
            json!({"direction":direction, "stage":stage, "error":message,
                   "thread":thread, "queries":queries.map(format::Queries::details)})
        });
        #[cfg(windows)]
        if stage == "select"
            || (stage == "format"
                && self.effective[usize::from(direction == "input")] != Some("by_id"))
        {
            probe::schedule(self.id, direction == "input", stage == "format", at);
        }
        message
    }
    pub fn config(
        &self,
        device: &cpal::Device,
        input: bool,
    ) -> Result<cpal::SupportedStreamConfig, String> {
        let name = format::name(device);
        let q = format::Queries::run(device, input);
        q.selected(input).ok_or_else(|| {
            self.fail(
                if input { "input" } else { "output" },
                "format",
                q.message(&name, input),
                Some(&q),
            )
        })
    }
}

fn thread_identity() -> Value {
    let thread = thread::current();
    json!({"name":thread.name(), "id":format!("{:?}",thread.id())})
}

pub(super) fn thread_context() -> Value {
    let v = thread_identity();
    #[cfg(windows)]
    let v = {
        use windows::Win32::System::Com::{APTTYPE, APTTYPEQUALIFIER, CoGetApartmentType};
        let mut v = v;
        let (mut apartment, mut qualifier) = (APTTYPE::default(), APTTYPEQUALIFIER::default());
        // Reads calling-thread state only. Never initializes COM.
        v["com"] = match unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) } {
            Ok(()) => json!({"type":apartment.0, "qualifier":qualifier.0}),
            Err(e) => json!({"hresult":format!("{:#010X}", e.code().0 as u32)}),
        };
        v
    };
    v
}
#[cfg(windows)]
mod probe;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_and_settings_log_permit_failures() {
        // The logger and stream permits are process-wide. Exercise both real
        // preparation workers without consuming another test's permits.
        const CHILD: &str = "THISCORD_TEST_OPEN_TRACE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "audio::open_trace::tests::recovery_and_settings_log_permit_failures",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let dir = std::env::temp_dir().join(format!("thiscord-open-trace-{}", std::process::id()));
        diagnostics::initialize(dir.clone(), dir.join("preference.json")).unwrap();
        diagnostics::set_enabled(true).unwrap();
        let _permits: Vec<_> = (0..4).map(|_| lifecycle::acquire().unwrap()).collect();
        let settings = AudioSettings::default();
        let mut recovery = recovery::Recovery::default();
        recovery.poll(&settings, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let read =
            || std::fs::read_to_string(dir.join("audio-diagnostics.jsonl")).unwrap_or_default();
        let wait = |count| loop {
            if read().matches("device_open_failed").count() >= count {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        };
        wait(1);
        let _pending = reconfigure::Pending::start(settings, true, false, None).unwrap();
        wait(2);
        diagnostics::set_enabled(false).unwrap();
        let exported = diagnostics::export().unwrap();
        let records: Vec<Value> = exported
            .last()
            .unwrap()
            .contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (caller, mic) in [("recovery", true), ("settings", false)] {
            let record = records
                .iter()
                .find(|v| v["event"] == "device_open_failed" && v["details"]["caller"] == caller)
                .unwrap();
            let d = &record["details"];
            assert_eq!(d["microphone"], mic);
            assert_eq!(d["stage"], "permit");
            assert!(
                records.iter().any(|v| v["event"] == "device_open_begin"
                    && v["details"]["open_id"] == d["open_id"])
            );
            for forbidden in [
                "guild_id",
                "account_id",
                "token",
                "sdp",
                "credential",
                "payload",
                "endpoint_id",
            ] {
                assert!(!record.to_string().contains(forbidden));
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
