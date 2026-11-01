use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tauri::Manager;
use thiscord_frontend::voice_overlay::{self as layout, Area};
use thiscord_shared::overlay::{
    OverlayAction, OverlayDisplay, OverlayLayout, OverlaySettings, OverlaySnapshot, OverlayStatus,
};

const FILE: &str = "overlay.json";
const MAX_FILE_BYTES: u64 = 4096;

#[derive(Default)]
pub struct OverlayState {
    settings: Mutex<OverlaySettings>,
    /// Serializes read-modify-write saves; never held by the visibility loop.
    persist: Mutex<()>,
    /// Session-only presentation state.
    hidden: AtomicBool,
    unlocked: AtomicBool,
    /// Bumped by placement changes so a visible overlay moves immediately.
    revision: AtomicU64,
    /// Set before showing and cleared after hiding, so snapshots (which query
    /// the audio worker) are only built while the overlay can be seen.
    visible: AtomicBool,
}
impl OverlayState {
    fn settings(&self) -> OverlaySettings {
        self.settings
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }
    fn layout(&self, settings: &OverlaySettings) -> OverlayLayout {
        OverlayLayout {
            corner: settings.corner,
            visibility_button: settings.visibility_button,
            hidden: self.hidden.load(Ordering::Relaxed),
            unlocked: settings.enabled && self.unlocked.load(Ordering::Relaxed),
        }
    }
}

fn load(app: &tauri::AppHandle) -> Result<OverlaySettings, String> {
    let path = app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?
        .join(FILE);
    if !path.exists() {
        return Ok(OverlaySettings::default());
    }
    if std::fs::metadata(&path)
        .map_err(|_| "Cannot read overlay settings")?
        .len()
        > MAX_FILE_BYTES
    {
        return Err("Overlay settings file is too large".into());
    }
    let settings: OverlaySettings =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "Cannot read overlay settings")?)
            .map_err(|_| "Invalid overlay settings")?;
    settings.validate()?;
    Ok(settings)
}

/// Blocking: validate, write atomically, then publish. Memory changes only
/// after the file is saved, so a failed save never shows as applied.
fn update(
    app: &tauri::AppHandle,
    change: impl FnOnce(&mut OverlaySettings) -> Result<(), String>,
) -> Result<OverlaySettings, String> {
    let state = app.state::<OverlayState>();
    let _guard = state
        .persist
        .lock()
        .map_err(|_| "Overlay settings unavailable")?;
    let mut next = state.settings();
    change(&mut next)?;
    next.validate()?;
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?;
    std::fs::create_dir_all(&directory).map_err(|_| "Cannot create settings directory")?;
    let temporary = directory.join(format!("{FILE}.tmp"));
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&next).map_err(|_| "Invalid overlay settings")?,
    )
    .map_err(|_| "Cannot save overlay settings")?;
    std::fs::rename(temporary, directory.join(FILE)).map_err(|_| "Cannot save overlay settings")?;
    if !next.enabled {
        state.unlocked.store(false, Ordering::Relaxed);
    }
    *state
        .settings
        .lock()
        .map_err(|_| "Overlay settings unavailable")? = next.clone();
    state.revision.fetch_add(1, Ordering::Relaxed);
    Ok(next)
}

async fn update_async(
    app: tauri::AppHandle,
    change: impl FnOnce(&mut OverlaySettings) -> Result<(), String> + Send + 'static,
) -> Result<OverlaySettings, String> {
    tauri::async_runtime::spawn_blocking(move || update(&app, change))
        .await
        .map_err(|_| "Overlay settings task failed")?
}

fn area(monitor: &tauri::Monitor) -> Area {
    let work = monitor.work_area();
    Area {
        x: work.position.x,
        y: work.position.y,
        width: work.size.width,
        height: work.size.height,
        scale: monitor.scale_factor(),
    }
}

/// The saved display, or the display containing Thiscord when it is absent.
fn target_monitor(
    main: &tauri::WebviewWindow,
    settings: &OverlaySettings,
) -> Option<tauri::Monitor> {
    settings
        .display
        .as_ref()
        .and_then(|name| {
            main.available_monitors()
                .ok()?
                .into_iter()
                .find(|m| m.name() == Some(name))
        })
        .or_else(|| main.current_monitor().ok().flatten())
        .or_else(|| main.primary_monitor().ok().flatten())
}

/// Saved placement equivalent to where the visible overlay is now, for
/// committing a drag. `None` when the overlay is hidden or the OS cannot
/// report window positions (for example Wayland).
fn dragged_placement(app: &tauri::AppHandle) -> Option<OverlaySettings> {
    // Wayland reports a fixed origin rather than the real window position.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return None;
    }
    let window = app.get_webview_window("voice-overlay")?;
    if !window.is_visible().ok()? {
        return None;
    }
    let position = window.outer_position().ok()?;
    let monitor = window.current_monitor().ok()??;
    let (corner, offset_x, offset_y) = layout::placement((position.x, position.y), area(&monitor));
    let mut settings = app.state::<OverlayState>().settings();
    // Keep the saved display choice (including "follow Thiscord" and a saved
    // display that is disconnected) unless the overlay moved to another one.
    let expected = app
        .get_webview_window("main")
        .and_then(|main| target_monitor(&main, &settings))
        .and_then(|m| m.name().cloned());
    if monitor.name().is_some() && monitor.name() != expected.as_ref() {
        settings.display = monitor.name().cloned();
    }
    settings.corner = corner;
    settings.offset_x = offset_x;
    settings.offset_y = offset_y;
    Some(settings)
}

pub fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    match load(app) {
        Ok(settings) => {
            if let Ok(mut current) = app.state::<OverlayState>().settings.lock() {
                *current = settings;
            }
        }
        Err(error) => eprintln!("{error}; using default overlay settings"),
    }
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "voice-overlay",
        tauri::WebviewUrl::App("index.html#voice-overlay".into()),
    )
    .title("Thiscord voice overlay")
    .inner_size(layout::WIDTH, layout::HEIGHT)
    .position(24.0, 48.0)
    .transparent(true)
    .decorations(false)
    .shadow(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .focusable(false)
    .visible(false)
    .build()?;
    if let Err(error) = window.set_ignore_cursor_events(true) {
        let _ = window.close();
        return Err(error);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut placed = None;
        // Cursor passthrough as last applied; `None` forces the next update.
        let mut applied_interactive = Some(false);
        loop {
            let Some(main) = app.get_webview_window("main") else {
                let _ = window.close();
                break;
            };
            let state = app.state::<OverlayState>();
            let settings = state.settings();
            let view = state.layout(&settings);
            let voice = crate::native_voice::voice_status(app.state()).ok();
            let connected = voice.as_ref().is_some_and(|s| {
                s.connected && s.channel_id.is_some() && !s.participants.is_empty()
            });
            // Unlocking shows the overlay over Thiscord, even outside voice, so it
            // can be positioned. Collapsed overlays keep only the optional button.
            let show = settings.enabled
                && (view.unlocked
                    || (connected
                        && !main.is_focused().unwrap_or(true)
                        && (!view.hidden || view.visibility_button)));
            let revision = state.revision.load(Ordering::Relaxed);
            let visible = window.is_visible().unwrap_or(false);
            if show && (!visible || placed != Some(revision)) {
                if let Some(monitor) = target_monitor(&main, &settings) {
                    let (x, y) = layout::origin(&settings, area(&monitor));
                    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
                }
                placed = Some(revision);
            }
            if visible != show {
                if show {
                    state.visible.store(true, Ordering::Relaxed);
                    let _ = window.show();
                } else {
                    let _ = window.hide();
                    state.visible.store(false, Ordering::Relaxed);
                }
                applied_interactive = None;
            }
            // Query the OS cursor because a click-through WebView cannot receive
            // pointer-enter events. Positioning mode accepts input everywhere.
            let has_self = voice
                .as_ref()
                .is_some_and(|s| s.participants.iter().any(|p| Some(p.slot) == s.own_slot));
            let interactive = show
                && (view.unlocked
                    || window
                        .cursor_position()
                        .ok()
                        .zip(window.inner_position().ok())
                        .zip(window.scale_factor().ok())
                        .is_some_and(|((cursor, origin), scale)| {
                            layout::hit(
                                view,
                                has_self && connected,
                                (cursor.x - f64::from(origin.x)) / scale,
                                (cursor.y - f64::from(origin.y)) / scale,
                            )
                            .is_some()
                        }));
            if applied_interactive != Some(interactive) {
                applied_interactive = window
                    .set_ignore_cursor_events(!interactive)
                    .is_ok()
                    .then_some(interactive);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    Ok(())
}

fn available(app: &tauri::AppHandle) -> Result<(), String> {
    if app.get_webview_window("voice-overlay").is_none() {
        return Err("Voice overlay is unavailable on this display server".into());
    }
    Ok(())
}

/// Shared by global shortcuts, the overlay window and Settings.
pub async fn action(app: tauri::AppHandle, action: OverlayAction) -> Result<(), String> {
    available(&app)?;
    let state = app.state::<OverlayState>();
    match action {
        OverlayAction::ToggleVisibility => {
            state.hidden.fetch_xor(true, Ordering::Relaxed);
        }
        OverlayAction::NextCorner => {
            // Keep a pending drag's distances when rotating while unlocked.
            let unlocked = state.layout(&state.settings()).unlocked;
            let dragged = unlocked.then(|| dragged_placement(&app)).flatten();
            update_async(app.clone(), move |settings| {
                if let Some(dragged) = dragged {
                    *settings = dragged;
                }
                settings.corner = settings.corner.next();
                Ok(())
            })
            .await?;
        }
        OverlayAction::ToggleUnlock => {
            let settings = state.settings();
            if state.layout(&settings).unlocked {
                if let Some(dragged) = dragged_placement(&app)
                    && dragged != settings
                {
                    update_async(app.clone(), move |settings| {
                        *settings = dragged;
                        Ok(())
                    })
                    .await?;
                }
                state.unlocked.store(false, Ordering::Relaxed);
            } else if !settings.enabled {
                return Err("Turn on the overlay before moving it".into());
            } else {
                state.unlocked.store(true, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn overlay_action(app: tauri::AppHandle, action: OverlayAction) -> Result<(), String> {
    self::action(app, action).await
}

/// Begin an OS window move from the overlay's positioning surface.
#[tauri::command]
pub fn overlay_start_drag(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<OverlayState>();
    if !state.layout(&state.settings()).unlocked {
        return Err("Unlock the overlay to move it".into());
    }
    app.get_webview_window("voice-overlay")
        .ok_or("Voice overlay is unavailable on this display server")?
        .start_dragging()
        .map_err(|_| "This desktop does not allow moving the overlay".into())
}

#[tauri::command]
pub fn overlay_settings(app: tauri::AppHandle) -> Result<OverlayStatus, String> {
    available(&app)?;
    let state = app.state::<OverlayState>();
    let settings = state.settings();
    let main = app.get_webview_window("main");
    let primary = main
        .as_ref()
        .and_then(|w| w.primary_monitor().ok().flatten())
        .and_then(|m| m.name().cloned());
    let displays = main
        .and_then(|w| w.available_monitors().ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            let name = m.name()?.clone();
            let size = m.size().to_logical::<u32>(m.scale_factor());
            Some(OverlayDisplay {
                primary: primary.as_ref() == Some(&name),
                name,
                width: size.width,
                height: size.height,
            })
        })
        .collect();
    Ok(OverlayStatus {
        layout: state.layout(&settings),
        settings,
        displays,
    })
}

#[tauri::command]
pub async fn overlay_settings_save(
    app: tauri::AppHandle,
    settings: OverlaySettings,
) -> Result<(), String> {
    available(&app)?;
    settings.validate()?;
    update_async(app, move |current| {
        *current = settings;
        Ok(())
    })
    .await
    .map(|_| ())
}

/// Admin relay: only the on/off preference, never placement or positioning.
pub fn overlay_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    available(&app)?;
    Ok(app.state::<OverlayState>().settings().enabled)
}

pub async fn overlay_enable(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    available(&app)?;
    update_async(app, move |settings| {
        settings.enabled = enabled;
        Ok(())
    })
    .await
    .map(|_| ())
}

/// `None` while the overlay is hidden: the WebView keeps its last snapshot and
/// no audio-worker status is computed for a roster nobody can see.
#[tauri::command]
pub async fn overlay_snapshot(app: tauri::AppHandle) -> Result<Option<OverlaySnapshot>, String> {
    let state = app.state::<OverlayState>();
    if !state.visible.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let settings = state.settings();
    let layout = state.layout(&settings);
    let idle = OverlaySnapshot {
        layout,
        ..Default::default()
    };
    // A collapsed roster shows only the button; skip the audio worker for it.
    if layout.hidden && !layout.unlocked {
        return Ok(Some(idle));
    }
    let before = crate::native_voice::voice_status(app.state())?;
    if !before.connected || !settings.enabled {
        return Ok(Some(idle));
    }
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    let audio = tauri::async_runtime::spawn_blocking(move || {
        // Only speaking/mute presentation is needed; skip the quality report.
        engine.command(thiscord_frontend::audio::Command::Peek)
    })
    .await
    .map_err(|_| "Audio worker unavailable")??;
    let audio_settings = crate::native_audio::audio_current(app.clone()).await?;
    let voice = crate::native_voice::voice_status(app.state())?;
    if !voice.connected || voice.channel_id != before.channel_id {
        return Ok(Some(idle));
    }
    let mut snapshot = thiscord_frontend::voice_overlay::snapshot(&voice, &audio);
    for person in snapshot.participants.iter_mut().filter(|p| p.is_self) {
        person.muted = audio_settings.muted;
        person.deafened = audio_settings.deafened;
        person.speaking &= !audio_settings.muted && !audio_settings.deafened;
    }
    snapshot.layout = layout;
    Ok(Some(snapshot))
}

#[tauri::command]
pub async fn overlay_toggle_audio(
    app: tauri::AppHandle,
    action: thiscord_shared::audio::AudioToggle,
) -> Result<(), String> {
    let voice = crate::native_voice::voice_status(app.state())?;
    let state = app.state::<OverlayState>();
    if !voice.connected || voice.own_slot.is_none() || !state.settings().enabled {
        return Err("Join voice to use overlay controls".into());
    }
    crate::native_audio::audio_toggle(app, action).await
}
