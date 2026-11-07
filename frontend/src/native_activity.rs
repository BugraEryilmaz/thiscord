//! Detects running games for the "Playing …" status (docs/activity.md).
//!
//! A background thread lists local process names every [`SCAN`] while detection
//! is enabled and matches them with `game_activity`, after the games added on
//! this device and without hidden ones. Process names never leave the device:
//! the UI receives the matched game, and the running-program list only while
//! adding a game. Preferences are a device-local file (on unless switched off).
use std::{
    path::PathBuf,
    sync::{Condvar, Mutex, MutexGuard},
    time::Duration,
};
use tauri::{Emitter, Manager};
use thiscord_frontend::game_activity::{
    self, ACTIVITY_EVENT, ActivityStatus, CustomGame, Detected, LEAGUE_OF_LEGENDS, Preferences,
    RunningProgram,
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
fn load(path: &std::path::Path) -> Preferences {
    std::fs::read(path)
        .map(|data| Preferences::parse(&data))
        .unwrap_or_default()
}

fn save(path: &std::path::Path, preferences: &Preferences) -> Result<(), String> {
    let directory = path.parent().ok_or("Settings directory unavailable")?;
    std::fs::create_dir_all(directory).map_err(|_| "Cannot create settings directory")?;
    let temporary = path.with_extension("tmp");
    // Without lists, keep the bare switch older versions read, so going back to
    // one never turns sharing on.
    let data = if preferences.games.is_empty() && preferences.hidden.is_empty() {
        serde_json::to_vec(&preferences.enabled)
    } else {
        serde_json::to_vec_pretty(preferences)
    }
    .map_err(|_| "Cannot save activity settings")?;
    std::fs::write(&temporary, data).map_err(|_| "Cannot save activity settings")?;
    std::fs::rename(temporary, path).map_err(|_| "Cannot save activity settings".into())
}

pub fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    let path = app.path().app_config_dir()?.join("activity.json");
    let Preferences {
        enabled,
        games,
        hidden,
    } = load(&path);
    app.manage(ActivityState {
        inner: Mutex::new(Inner {
            status: ActivityStatus {
                enabled,
                activity: None,
                games,
                hidden,
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
        let (enabled, games, hidden) = {
            let inner = state.lock();
            let status = &inner.status;
            (status.enabled, status.games.clone(), status.hidden.clone())
        };
        let detected = if enabled {
            detect(&mut league, &games, &hidden)
        } else {
            league = None;
            None
        };
        let mut inner = state.lock();
        // Disabling during a scan wins; enabling or editing games sets `woken`,
        // so a result from the old lists is skipped and replaced by the rescan.
        let detected = detected.filter(|_| inner.status.enabled);
        let changed = (!inner.woken && inner.status.activity != detected).then(|| {
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

fn detect(
    league: &mut Option<&'static str>,
    games: &[CustomGame],
    hidden: &[String],
) -> Option<Activity> {
    match game_activity::detect_with(processes::names(), games, hidden) {
        Some(Detected::LeagueMatch) => {
            if league.is_none() {
                *league = league_mode();
            }
            let mode = league.unwrap_or(LEAGUE_OF_LEGENDS);
            (!hidden.iter().any(|name| name == mode)).then(|| Activity::playing(mode))
        }
        detected => {
            *league = None;
            match detected? {
                Detected::Custom(index) => Some(Activity::playing(games.get(index)?.name.clone())),
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
            save(
                &state.path,
                &Preferences {
                    enabled,
                    games: inner.status.games.clone(),
                    hidden: inner.status.hidden.clone(),
                },
            )?;
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

/// Replaces the games added on this device and the hidden built-in games, and
/// rescans right away.
#[tauri::command]
pub async fn activity_set_games(
    app: tauri::AppHandle,
    games: Vec<CustomGame>,
    hidden: Vec<String>,
) -> Result<ActivityStatus, String> {
    let (games, hidden) = game_activity::validate(games, hidden).map_err(str::to_owned)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ActivityState>();
        let status = {
            // Held across the write so concurrent changes save in order.
            let mut inner = state.lock();
            save(
                &state.path,
                &Preferences {
                    enabled: inner.status.enabled,
                    games: games.clone(),
                    hidden: hidden.clone(),
                },
            )?;
            inner.status.games = games;
            inner.status.hidden = hidden;
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

/// Programs running in this user's session, for choosing a game to add. Only
/// requested while the picker is open; the list stays on this device.
#[tauri::command]
pub async fn activity_programs() -> Result<Vec<RunningProgram>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let own = std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name()?.to_str().map(game_activity::process_key))
            .unwrap_or_default();
        game_activity::running_programs(processes::programs(), &own)
    })
    .await
    .map_err(|_| "Could not list running programs".to_owned())
}

/// Executable names or paths of running processes, as far as this user can see.
mod processes {
    #[cfg(target_os = "windows")]
    pub fn names() -> Vec<String> {
        entries().into_iter().map(|(_, name)| name).collect()
    }

    /// Programs with a visible, titled top-level window: what people see as
    /// "running apps", including games, without background services.
    #[cfg(target_os = "windows")]
    pub fn programs() -> Vec<String> {
        use std::collections::HashSet;
        use windows::Win32::{
            Foundation::{HWND, LPARAM},
            UI::WindowsAndMessaging::{
                EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId,
                IsWindowVisible,
            },
        };
        unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> windows::core::BOOL {
            // SAFETY: `data` points to the set below for the duration of EnumWindows.
            let pids = unsafe { &mut *(data.0 as *mut HashSet<u32>) };
            // SAFETY: plain queries on a window handle EnumWindows just provided.
            unsafe {
                if IsWindowVisible(window).as_bool()
                    && GetWindow(window, GW_OWNER).is_err()
                    && GetWindowTextLengthW(window) > 0
                {
                    let mut pid = 0;
                    GetWindowThreadProcessId(window, Some(&mut pid));
                    if pid != 0 {
                        pids.insert(pid);
                    }
                }
            }
            true.into()
        }
        let mut pids = HashSet::new();
        // SAFETY: the callback only dereferences the set, which outlives the call.
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut pids as *mut HashSet<u32> as isize));
        }
        entries()
            .into_iter()
            .filter(|(pid, _)| pids.contains(pid))
            .map(|(_, name)| name)
            .collect()
    }

    #[cfg(target_os = "windows")]
    fn entries() -> Vec<(u32, String)> {
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
                names.push((
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&file[..length]),
                ));
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
        scan(false)
    }

    /// This user's programs, without system services and helpers.
    #[cfg(target_os = "linux")]
    pub fn programs() -> Vec<String> {
        scan(true)
    }

    #[cfg(target_os = "linux")]
    fn scan(own_programs: bool) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        // `/proc/<pid>` is owned by the process's user; compare with our own.
        let uid = std::fs::metadata("/proc/self").map_or(u32::MAX, |m| m.uid());
        const SYSTEM: &[&str] = &[
            "/usr/lib/",
            "/usr/libexec/",
            "/usr/sbin/",
            "/lib/",
            "/sbin/",
        ];
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
            if own_programs {
                let system = std::fs::read_link(path.join("exe")).map_or(true, |exe| {
                    let exe = exe.to_string_lossy();
                    SYSTEM.iter().any(|prefix| exe.starts_with(prefix)) && !exe.contains("steam")
                });
                if system || entry.metadata().map_or(true, |m| m.uid() != uid) {
                    continue;
                }
            }
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

    /// Programs outside the operating system's own directories.
    #[cfg(target_os = "macos")]
    pub fn programs() -> Vec<String> {
        const SYSTEM: &[&str] = &[
            "/System/",
            "/usr/",
            "/bin/",
            "/sbin/",
            "/Library/Apple/",
            "/private/",
        ];
        names()
            .into_iter()
            .filter(|path| !SYSTEM.iter().any(|prefix| path.starts_with(prefix)))
            .collect()
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    pub fn names() -> Vec<String> {
        Vec::new()
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    pub fn programs() -> Vec<String> {
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
        assert_eq!(load(&path), Preferences::default());
        let saved = Preferences {
            enabled: false,
            games: vec![CustomGame {
                name: "Hollow Knight".into(),
                process: "hollow_knight".into(),
            }],
            hidden: vec!["Factorio".into()],
        };
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), saved);
        // Without lists, the file stays the bare switch older versions read.
        save(
            &path,
            &Preferences {
                enabled: false,
                ..Preferences::default()
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "false");
        // Files written by older versions hold only the switch.
        std::fs::write(&path, "false").unwrap();
        assert!(!load(&path).enabled);
        std::fs::write(&path, "invalid").unwrap();
        assert_eq!(load(&path), Preferences::default());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn this_process_is_listed() {
        let names = processes::names();
        let own = std::env::current_exe().unwrap();
        let own = game_activity::process_key(own.file_name().unwrap().to_str().unwrap());
        // Programs for the picker are a subset of what the scanner sees.
        let programs = processes::programs();
        assert!(programs.iter().all(|p| {
            names
                .iter()
                .any(|n| game_activity::process_key(n) == game_activity::process_key(p))
        }));
        assert!(
            names.iter().any(|n| game_activity::process_key(n) == own),
            "{own} missing from {} processes",
            names.len()
        );
    }
}
