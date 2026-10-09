//! Bounded, asynchronous local diagnostics. Never pass frames, credentials or account data.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 2 * 1024 * 1024;
const COPIES: usize = 3;
static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn initialize(directory: PathBuf, preference: PathBuf) -> Result<(), String> {
    if LOGGER.get().is_some() {
        return Ok(());
    }
    let logger = Logger::new(directory, preference);
    let restored = logger.restore();
    let _ = LOGGER.set(logger);
    application_details();
    // Do not log panic payloads: they can contain arbitrary application data.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Never block a panic on the logger lock or capture stacks while off.
        if LOGGER
            .get()
            .is_some_and(|logger| logger.active.try_lock().is_ok_and(|s| s.is_some()))
        {
            let location = info.location().map(|l| serde_json::json!({"file":Path::new(l.file()).file_name().and_then(|s|s.to_str()),"line":l.line()}));
            let stack: String = std::backtrace::Backtrace::force_capture()
                .to_string()
                .chars()
                .take(8000)
                .collect();
            event(
                "panic",
                serde_json::json!({"location":location,"thread":std::thread::current().name(),"stack":stack}),
            );
        }
        previous(info);
    }));
    restored
}

fn application_details() {
    event(
        "diagnostic_logging_started",
        serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"pid":std::process::id(),"logical_cpus":std::thread::available_parallelism().ok().map(|n|n.get()),"cpal":"0.18.2","deep_filter":cfg!(feature="deep-filter"),"neural_echo":cfg!(feature="neural-echo")}),
    );
}

pub fn directory() -> Result<PathBuf, String> {
    LOGGER
        .get()
        .map(|logger| logger.directory.clone())
        .ok_or_else(|| "Audio diagnostics could not be initialized".into())
}

pub fn enabled() -> Result<bool, String> {
    let logger = LOGGER.get().ok_or("Audio diagnostics unavailable")?;
    Ok(logger
        .active
        .lock()
        .map_err(|_| "Diagnostics settings unavailable")?
        .is_some())
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let logger = LOGGER.get().ok_or("Audio diagnostics unavailable")?;
    logger.set_enabled(enabled)?;
    if enabled {
        application_details();
    }
    Ok(())
}

/// Call only on control/background threads, never from a realtime callback.
pub fn event(name: &'static str, details: serde_json::Value) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    logger.event(name, details);
}

struct Logger {
    directory: PathBuf,
    preference: PathBuf,
    active: Mutex<Option<Session>>,
}

impl Logger {
    fn new(directory: PathBuf, preference: PathBuf) -> Self {
        Self {
            directory,
            preference,
            active: Mutex::new(None),
        }
    }

    fn restore(&self) -> Result<(), String> {
        // Bound reads and fail closed for missing, unreadable or damaged settings.
        let mut data = Vec::new();
        let opted_in = File::open(&self.preference)
            .and_then(|file| file.take(16).read_to_end(&mut data))
            .is_ok()
            && data == b"true";
        if opted_in {
            *self
                .active
                .lock()
                .map_err(|_| "Diagnostics settings unavailable")? =
                Some(Session::start(&self.directory)?);
        }
        Ok(())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "Diagnostics settings unavailable")?;
        let next = if enabled && active.is_none() {
            Some(Session::start(&self.directory)?)
        } else {
            None
        };
        let parent = self
            .preference
            .parent()
            .ok_or("Invalid diagnostics settings path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create settings directory")?;
        let temporary = self.preference.with_extension("tmp");
        fs::write(
            &temporary,
            if enabled {
                b"true".as_slice()
            } else {
                b"false".as_slice()
            },
        )
        .and_then(|()| fs::rename(temporary, &self.preference))
        .map_err(|_| "Cannot save diagnostics settings")?;
        if !enabled {
            // Drop joins the writer: no queued records can leak into a later session,
            // and no writes remain in flight when the command succeeds.
            active.take();
        } else if next.is_some() {
            *active = next;
        }
        Ok(())
    }

    fn event(&self, name: &'static str, details: serde_json::Value) {
        // Settings and disk operations must never stall audio control threads.
        let Ok(active) = self.active.try_lock() else {
            return;
        };
        let Some(session) = active.as_ref() else {
            return;
        };
        let line = serde_json::json!({"unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),"event":name,"details":details}).to_string();
        // Bound each record as well as queue depth and retained disk usage.
        let line = if line.len() <= 16 * 1024 {
            line
        } else {
            serde_json::json!({"event":name,"details_omitted":"record too large"}).to_string()
        };
        if session.sender.as_ref().unwrap().try_send(line).is_err() {
            session.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Session {
    sender: Option<mpsc::SyncSender<String>>,
    stopped: Arc<AtomicBool>,
    lost: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    fn start(directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(|_| "Cannot create audio diagnostics folder")?;
        let mut writer =
            Writer::new(directory.to_owned()).map_err(|_| "Cannot open audio diagnostics log")?;
        let (sender, rx) = mpsc::sync_channel::<String>(512);
        let stopped = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicU64::new(0));
        let stop = stopped.clone();
        let dropped = lost.clone();
        let worker = std::thread::Builder::new()
            .name("thiscord-audio-log".into())
            .spawn(move || {
                while let Ok(line) = rx.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let lost = dropped.swap(0, Ordering::Relaxed);
                    if lost > 0 {
                        let _ = writer.write(&format!(
                            "{{\"event\":\"diagnostic_queue_overflow\",\"dropped\":{lost}}}"
                        ));
                    }
                    if writer.write(&line).is_err() {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .map_err(|_| "Cannot start audio diagnostics writer")?;
        Ok(Self {
            sender: Some(sender),
            stopped,
            lost,
            worker: Some(worker),
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Writer {
    directory: PathBuf,
    file: Option<File>,
    bytes: u64,
}
impl Writer {
    fn new(directory: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("audio-diagnostics.jsonl"))?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            directory,
            file: Some(file),
            bytes,
        })
    }
    fn write(&mut self, line: &str) -> std::io::Result<()> {
        if self.bytes + line.len() as u64 + 1 > LIMIT {
            self.file.take();
            let oldest = self
                .directory
                .join(format!("audio-diagnostics.{}.jsonl", COPIES - 1));
            if oldest.exists() {
                fs::remove_file(oldest)?;
            }
            for i in (1..COPIES).rev() {
                let from = self.directory.join(if i == 1 {
                    "audio-diagnostics.jsonl".into()
                } else {
                    format!("audio-diagnostics.{}.jsonl", i - 1)
                });
                if from.exists() {
                    fs::rename(
                        from,
                        self.directory.join(format!("audio-diagnostics.{i}.jsonl")),
                    )?;
                }
            }
            self.bytes = 0;
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.directory.join("audio-diagnostics.jsonl"))?,
            );
        }
        let file = self.file.as_mut().expect("file opened above");
        writeln!(file, "{line}")?;
        file.flush()?;
        self.bytes += line.len() as u64 + 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> PathBuf {
        std::env::temp_dir().join(format!(
            "thiscord-logging-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn wait_for(directory: &Path, marker: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !fs::read_to_string(directory.join("audio-diagnostics.jsonl"))
            .unwrap_or_default()
            .contains(marker)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "diagnostic event was not written"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn opt_in_persists_and_disable_stops_writes_without_deleting_logs() {
        let root = temporary();
        let directory = root.join("logs");
        let preference = root.join("config/diagnostics.json");
        let logger = Logger::new(directory.clone(), preference.clone());
        logger.restore().unwrap();
        logger.event("off_by_default", serde_json::json!({}));
        assert!(!root.exists());
        logger.set_enabled(true).unwrap();
        logger.event("first_session", serde_json::json!({}));
        wait_for(&directory, "first_session");
        drop(logger);

        let logger = Logger::new(directory.clone(), preference.clone());
        logger.restore().unwrap();
        assert!(logger.active.lock().unwrap().is_some());
        logger.event("after_restart", serde_json::json!({}));
        wait_for(&directory, "after_restart");
        for _ in 0..1000 {
            logger.event("queued", serde_json::json!({}));
        }
        logger.set_enabled(false).unwrap();
        let file = directory.join("audio-diagnostics.jsonl");
        let before = fs::read(&file).unwrap();
        logger.event("disabled_event", serde_json::json!({}));
        assert_eq!(before, fs::read(&file).unwrap());
        drop(logger);

        let logger = Logger::new(directory.clone(), preference);
        logger.restore().unwrap();
        assert!(logger.active.lock().unwrap().is_none());
        logger.set_enabled(true).unwrap();
        logger.event("reenabled", serde_json::json!({}));
        wait_for(&directory, "reenabled");
        logger.set_enabled(false).unwrap();
        let log = fs::read_to_string(file).unwrap();
        assert!(log.contains("first_session"));
        assert!(!log.contains("off_by_default"));
        assert!(!log.contains("disabled_event"));
        assert!(!log[before.len()..].contains("queued"));
        drop(logger);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_preferences_and_failed_saves_do_not_enable_logging() {
        let root = temporary();
        fs::create_dir_all(&root).unwrap();
        let preference = root.join("diagnostics.json");
        fs::write(&preference, "invalid").unwrap();
        let logger = Logger::new(root.join("logs"), preference);
        logger.restore().unwrap();
        assert!(logger.active.lock().unwrap().is_none());
        assert!(!logger.directory.exists());
        fs::create_dir(root.join("diagnostics.tmp")).unwrap();
        assert!(logger.set_enabled(true).is_err());
        assert!(logger.active.lock().unwrap().is_none());
        fs::remove_dir(root.join("diagnostics.tmp")).unwrap();
        logger.set_enabled(true).unwrap();
        fs::create_dir(root.join("diagnostics.tmp")).unwrap();
        assert!(logger.set_enabled(false).is_err());
        assert!(logger.active.lock().unwrap().is_some());
        assert_eq!(fs::read(&logger.preference).unwrap(), b"true");
        drop(logger);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_preserves_recent_logs_across_restart_and_bounds_disk_use() {
        let directory = std::env::temp_dir().join(format!(
            "thiscord-log-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let mut writer = Writer::new(directory.clone()).unwrap();
        for i in 0..5 {
            writer.bytes = LIMIT;
            writer.write(&format!("{{\"sequence\":{i}}}")).unwrap();
        }
        drop(writer);
        Writer::new(directory.clone())
            .unwrap()
            .write("{\"restart\":true}")
            .unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), COPIES);
        let latest = fs::read_to_string(directory.join("audio-diagnostics.jsonl")).unwrap();
        assert!(latest.contains("\"sequence\":4"));
        assert!(latest.contains("restart"));
        fs::remove_dir_all(directory).unwrap();
    }
}
