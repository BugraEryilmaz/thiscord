//! Window lifetime and a local, opt-in close-to-tray preference.
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

pub struct WindowState {
    enabled: AtomicBool,
    file_lock: Mutex<()>,
    path: PathBuf,
}

impl WindowState {
    fn load(path: PathBuf) -> Self {
        // Missing or damaged preferences retain the normal close behavior.
        let enabled = std::fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice::<bool>(&data).ok())
            .unwrap_or(false);
        Self {
            enabled: AtomicBool::new(enabled),
            file_lock: Mutex::new(()),
            path,
        }
    }

    fn save(&self, enabled: bool) -> Result<(), String> {
        let _guard = self
            .file_lock
            .lock()
            .map_err(|_| "Window settings unavailable")?;
        let directory = self.path.parent().ok_or("Settings directory unavailable")?;
        std::fs::create_dir_all(directory).map_err(|_| "Cannot create settings directory")?;
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, if enabled { "true" } else { "false" })
            .map_err(|_| "Cannot save window settings")?;
        std::fs::rename(temporary, &self.path).map_err(|_| "Cannot save window settings")?;
        self.enabled.store(enabled, Ordering::Release);
        Ok(())
    }
}

pub fn restore(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    app.manage(WindowState::load(
        app.path().app_config_dir()?.join("close-to-tray.json"),
    ));
    let open = MenuItem::with_id(app, "tray-open", "Open Thiscord", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "Quit Thiscord", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("thiscord")
        .tooltip("Thiscord")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-open" => restore(app),
            "tray-quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn close_to_tray(app: &tauri::AppHandle) -> bool {
    app.tray_by_id("thiscord").is_some()
        && app
            .try_state::<WindowState>()
            .is_some_and(|s| s.enabled.load(Ordering::Acquire))
}

#[tauri::command]
pub fn window_close_to_tray(app: tauri::AppHandle) -> Result<bool, String> {
    if app.tray_by_id("thiscord").is_none() {
        return Err("The system tray is unavailable on this computer".into());
    }
    Ok(close_to_tray(&app))
}

#[tauri::command]
pub async fn window_set_close_to_tray(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    window_close_to_tray(app.clone())?;
    tauri::async_runtime::spawn_blocking(move || app.state::<WindowState>().save(enabled))
        .await
        .map_err(|_| "Window settings task failed")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_persists_and_failed_save_keeps_current_behavior() {
        let directory =
            std::env::temp_dir().join(format!("thiscord-window-{}", rand::random::<u64>()));
        let path = directory.join("close-to-tray.json");
        let state = WindowState::load(path.clone());
        assert!(!state.enabled.load(Ordering::Acquire));
        state.save(true).unwrap();
        assert!(
            WindowState::load(path.clone())
                .enabled
                .load(Ordering::Acquire)
        );
        state.save(false).unwrap();
        assert!(
            !WindowState::load(path.clone())
                .enabled
                .load(Ordering::Acquire)
        );
        std::fs::write(&path, "invalid").unwrap();
        assert!(
            !WindowState::load(path.clone())
                .enabled
                .load(Ordering::Acquire)
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(state.save(true).is_err());
        assert!(!state.enabled.load(Ordering::Acquire));
        std::fs::remove_file(path.with_extension("tmp")).unwrap();
        std::fs::remove_dir(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
