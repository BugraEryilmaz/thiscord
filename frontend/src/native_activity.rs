//! Detects running games for the "Playing …" status (docs/activity.md).
//!
//! A background thread lists local process names every [`SCAN`] while detection
//! is enabled and matches them with `game_activity`. Process names never leave
//! this module; the UI receives only the matched game, and the preference is a
//! device-local file (on unless switched off).
use std::{
    path::PathBuf,
    sync::{Condvar, Mutex, MutexGuard},
    time::Duration,
};
use tauri::{Emitter, Manager};
use thiscord_frontend::game_activity::{
    self, ACTIVITY_EVENT, ActivityStatus, Detected, LEAGUE_OF_LEGENDS,
};
use thiscord_shared::account::Activity;

const SCAN: Duration = Duration::from_secs(15);

pub struct ActivityState {
    inner: Mutex<Inner>,
    wake: Condvar,
    path: PathBuf,
}

struct Inner {
    status: ActivityStatus,
    /// A settings change asks for an immediate rescan.
    woken: bool,
}

impl ActivityState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Missing or damaged preferences keep detection on.
fn load(path: &std::path::Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<bool>(&data).ok())
        .unwrap_or(true)
}

fn save(path: &std::path::Path, enabled: bool) -> Result<(), String> {
    let directory = path.parent().ok_or("Settings directory unavailable")?;
    std::fs::create_dir_all(directory).map_err(|_| "Cannot create settings directory")?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, if enabled { "true" } else { "false" })
        .map_err(|_| "Cannot save activity settings")?;
    std::fs::rename(temporary, path).map_err(|_| "Cannot save activity settings".into())
}

pub fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    let path = app.path().app_config_dir()?.join("activity.json");
    let enabled = load(&path);
    app.manage(ActivityState {
        inner: Mutex::new(Inner {
            status: ActivityStatus {
                enabled,
                activity: None,
            },
            woken: false,
        }),
        wake: Condvar::new(),
        path,
    });
    let app = app.clone();
    std::thread::Builder::new()
        .name("thiscord-activity".into())
        .spawn(move || run(app))?;
    Ok(())
}

fn run(app: tauri::AppHandle) {
    let state = app.state::<ActivityState>();
    // The League mode of the current match, once its live client API answered.
    let mut league = None;
    loop {
        let enabled = state.lock().status.enabled;
        let detected = if enabled {
            detect(&mut league)
        } else {
            league = None;
            None
        };
        let mut inner = state.lock();
        // Disabling during a scan wins; enabling sets `woken` for a rescan.
        let detected = detected.filter(|_| inner.status.enabled);
        let changed = (inner.status.activity != detected).then(|| {
            inner.status.activity = detected;
            inner.status.clone()
        });
        if let Some(status) = changed {
            drop(inner);
            let _ = app.emit(ACTIVITY_EVENT, status);
            inner = state.lock();
        }
        let (mut inner, _) = state
            .wake
            .wait_timeout_while(inner, SCAN, |inner| !inner.woken)
            .unwrap_or_else(|e| e.into_inner());
        inner.woken = false;
    }
}

fn detect(league: &mut Option<&'static str>) -> Option<Activity> {
    match game_activity::detect(processes::names()) {
        Some(Detected::LeagueMatch) => {
            if league.is_none() {
                *league = league_mode();
            }
            Some(Activity::playing(league.unwrap_or(LEAGUE_OF_LEGENDS)))
        }
        detected => {
            *league = None;
            match detected? {
                Detected::Game(name) => Some(Activity::playing(name)),
                Detected::LeagueMatch => None,
            }
        }
    }
}

/// League of Legends and Teamfight Tactics share one game executable; the
/// match's loopback live client API names the mode once the match has loaded.
/// It serves Riot's self-signed certificate. Nothing is sent to it, and only a
/// fixed label is derived from the reply, so the certificate is not verified.
fn league_mode() -> Option<&'static str> {
    tauri::async_runtime::block_on(async {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;
        let response = client
            .get("https://127.0.0.1:2999/liveclientdata/gamestats")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        if response
            .content_length()
            .is_some_and(|length| length > 64 * 1024)
        {
            return None;
        }
        game_activity::league_mode(&response.bytes().await.ok()?)
    })
}

#[tauri::command]
pub fn activity_status(state: tauri::State<'_, ActivityState>) -> ActivityStatus {
    state.lock().status.clone()
}

#[tauri::command]
pub async fn activity_set_enabled(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<ActivityStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ActivityState>();
        let status = {
            // Held across the write so concurrent changes save in order.
            let mut inner = state.lock();
            save(&state.path, enabled)?;
            inner.status.enabled = enabled;
            if !enabled {
                inner.status.activity = None;
            }
            inner.woken = true;
            inner.status.clone()
        };
        state.wake.notify_all();
        let _ = app.emit(ACTIVITY_EVENT, status.clone());
        Ok(status)
    })
    .await
    .map_err(|_| "Activity settings task failed")?
}

/// Executable names or paths of running processes, as far as this user can see.
mod processes {
    #[cfg(target_os = "windows")]
    pub fn names() -> Vec<String> {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
        };
        let mut names = Vec::new();
        // SAFETY: the snapshot handle is closed below, and every entry is
        // initialized with its size before the API writes into it.
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return names;
            };
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                let file = &entry.szExeFile;
                let length = file.iter().position(|&c| c == 0).unwrap_or(file.len());
                names.push(String::from_utf16_lossy(&file[..length]));
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
        }
        names
    }

    /// Native executables and Wine/Proton programs (whose first argument is the
    /// Windows path of the `.exe`).
    #[cfg(target_os = "linux")]
    pub fn names() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let path = entry.path();
            if let Ok(command) = std::fs::read(path.join("cmdline"))
                && let Some(first) = command.split(|&b| b == 0).next()
                && !first.is_empty()
            {
                names.push(String::from_utf8_lossy(first).into_owned());
            }
            if let Ok(executable) = std::fs::read_link(path.join("exe")) {
                names.push(executable.to_string_lossy().into_owned());
            }
        }
        names
    }

    #[cfg(target_os = "macos")]
    pub fn names() -> Vec<String> {
        let mut names = Vec::new();
        // SAFETY: buffers are sized in bytes as libproc expects, and returned
        // counts/lengths are clamped to those buffers.
        unsafe {
            let count = libc::proc_listallpids(std::ptr::null_mut(), 0);
            if count <= 0 {
                return names;
            }
            let mut pids = vec![0 as libc::pid_t; count as usize + 64];
            let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            let count = libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes);
            if count <= 0 {
                return names;
            }
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            for &pid in &pids[..(count as usize).min(pids.len())] {
                let length = libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32);
                if length > 0 {
                    let length = (length as usize).min(path.len());
                    names.push(String::from_utf8_lossy(&path[..length]).into_owned());
                }
            }
        }
        names
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    pub fn names() -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_defaults_on_and_round_trips() {
        let directory =
            std::env::temp_dir().join(format!("thiscord-activity-{}", rand::random::<u64>()));
        let path = directory.join("activity.json");
        assert!(load(&path));
        save(&path, false).unwrap();
        assert!(!load(&path));
        save(&path, true).unwrap();
        assert!(load(&path));
        std::fs::write(&path, "invalid").unwrap();
        assert!(load(&path));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn this_process_is_listed() {
        let names = processes::names();
        let own = std::env::current_exe().unwrap();
        let own = game_activity::process_key(own.file_name().unwrap().to_str().unwrap());
        assert!(
            names.iter().any(|n| game_activity::process_key(n) == own),
            "{own} missing from {} processes",
            names.len()
        );
    }
}
