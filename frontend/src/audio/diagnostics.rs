//! Bounded, asynchronous local diagnostics. Never pass frames, credentials or account data.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: u64 = 2 * 1024 * 1024;
const COPIES: usize = 3;
static LOGGER: OnceLock<mpsc::SyncSender<String>> = OnceLock::new();
static LOST: AtomicU64 = AtomicU64::new(0);
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

pub fn initialize(directory: PathBuf) -> Result<(), String> {
    if LOGGER.get().is_some() {
        return Ok(());
    }
    fs::create_dir_all(&directory).map_err(|_| "Cannot create audio diagnostics folder")?;
    let mut writer =
        Writer::new(directory.clone()).map_err(|_| "Cannot open audio diagnostics log")?;
    let (tx, rx) = mpsc::sync_channel::<String>(512);
    std::thread::Builder::new()
        .name("thiscord-audio-log".into())
        .spawn(move || {
            while let Ok(line) = rx.recv() {
                let lost = LOST.swap(0, Ordering::Relaxed);
                if lost > 0 {
                    let _ = writer.write(&format!(
                        "{{\"event\":\"diagnostic_queue_overflow\",\"dropped\":{lost}}}"
                    ));
                }
                if writer.write(&line).is_err() {
                    LOST.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
        .map_err(|_| "Cannot start audio diagnostics writer")?;
    let _ = DIRECTORY.set(directory);
    let _ = LOGGER.set(tx);
    event(
        "application_start",
        serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"pid":std::process::id(),"logical_cpus":std::thread::available_parallelism().ok().map(|n|n.get()),"cpal":"0.18.2","deep_filter":cfg!(feature="deep-filter"),"neural_echo":cfg!(feature="neural-echo")}),
    );
    // Do not log panic payloads: they can contain arbitrary application data.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
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
        previous(info);
    }));
    Ok(())
}

pub fn directory() -> Result<PathBuf, String> {
    DIRECTORY
        .get()
        .cloned()
        .ok_or_else(|| "Audio diagnostics could not be initialized".into())
}

/// Call only on control/background threads, never from a realtime callback.
pub fn event(name: &'static str, details: serde_json::Value) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    let line = serde_json::json!({"unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),"event":name,"details":details}).to_string();
    // Bound each record as well as queue depth and retained disk usage.
    let line = if line.len() <= 16 * 1024 {
        line
    } else {
        serde_json::json!({"event":name,"details_omitted":"record too large"}).to_string()
    };
    if logger.try_send(line).is_err() {
        LOST.fetch_add(1, Ordering::Relaxed);
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
