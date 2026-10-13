use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;
use thiscord_shared::overlay::OverlaySnapshot;

pub struct OverlayState {
    enabled: AtomicBool,
}
impl Default for OverlayState {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
        }
    }
}

pub fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "voice-overlay",
        tauri::WebviewUrl::App("index.html#voice-overlay".into()),
    )
    .title("Thiscord voice overlay")
    .inner_size(280.0, 560.0)
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
        loop {
            let Some(main) = app.get_webview_window("main") else {
                let _ = window.close();
                break;
            };
            let connected = crate::native_voice::voice_status(app.state())
                .is_ok_and(|s| s.connected && s.channel_id.is_some() && !s.participants.is_empty());
            let show = app.state::<OverlayState>().enabled.load(Ordering::Relaxed)
                && connected
                && !main.is_focused().unwrap_or(true);
            if window.is_visible().unwrap_or(false) != show {
                if show {
                    // Follow the display containing Thiscord, including negative origins.
                    if let Ok(Some(monitor)) = main.current_monitor() {
                        let origin = monitor.position();
                        let scale = monitor.scale_factor();
                        let _ = window.set_position(tauri::PhysicalPosition::new(
                            origin.x + (24.0 * scale) as i32,
                            origin.y + (48.0 * scale) as i32,
                        ));
                    }
                    let _ = window.show();
                } else {
                    let _ = window.hide();
                }
            }
            // The local row is first (6px padding, 268 x 52px). Query the OS cursor
            // because a click-through WebView cannot receive pointer-enter events.
            let interactive = show
                && crate::native_voice::voice_status(app.state())
                    .is_ok_and(|s| s.participants.iter().any(|p| Some(p.slot) == s.own_slot))
                && window
                    .cursor_position()
                    .ok()
                    .zip(window.inner_position().ok())
                    .zip(window.scale_factor().ok())
                    .is_some_and(|((cursor, origin), scale)| {
                        thiscord_frontend::voice_overlay::over_self_controls(
                            (cursor.x - f64::from(origin.x)) / scale,
                            (cursor.y - f64::from(origin.y)) / scale,
                        )
                    });
            let _ = window.set_ignore_cursor_events(!interactive);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    Ok(())
}

#[tauri::command]
pub fn overlay_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, OverlayState>,
) -> Result<bool, String> {
    if app.get_webview_window("voice-overlay").is_none() {
        return Err("Voice overlay is unavailable on this display server".into());
    }
    Ok(state.enabled.load(Ordering::Relaxed))
}

#[tauri::command]
pub fn overlay_enable(state: tauri::State<'_, OverlayState>, enabled: bool) {
    state.enabled.store(enabled, Ordering::Relaxed);
}

#[tauri::command]
pub async fn overlay_snapshot(app: tauri::AppHandle) -> Result<OverlaySnapshot, String> {
    let before = crate::native_voice::voice_status(app.state())?;
    if !before.connected || !app.state::<OverlayState>().enabled.load(Ordering::Relaxed) {
        return Ok(OverlaySnapshot::default());
    }
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    let audio = tauri::async_runtime::spawn_blocking(move || {
        engine.command(thiscord_frontend::audio::Command::Status)
    })
    .await
    .map_err(|_| "Audio worker unavailable")??;
    let settings = crate::native_audio::audio_current(app.clone()).await?;
    let voice = crate::native_voice::voice_status(app.state())?;
    if !voice.connected || voice.channel_id != before.channel_id {
        return Ok(OverlaySnapshot::default());
    }
    let mut snapshot = thiscord_frontend::voice_overlay::snapshot(&voice, &audio);
    for person in snapshot.participants.iter_mut().filter(|p| p.is_self) {
        person.muted = settings.muted;
        person.deafened = settings.deafened;
        person.speaking &= !settings.muted && !settings.deafened;
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn overlay_toggle_audio(
    app: tauri::AppHandle,
    action: thiscord_shared::audio::AudioToggle,
) -> Result<(), String> {
    let voice = crate::native_voice::voice_status(app.state())?;
    if !voice.connected
        || voice.own_slot.is_none()
        || !app.state::<OverlayState>().enabled.load(Ordering::Relaxed)
    {
        return Err("Join voice to use overlay controls".into());
    }
    crate::native_audio::audio_toggle(app, action).await
}
