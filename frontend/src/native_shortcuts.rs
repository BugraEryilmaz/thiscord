//! Global self controls. Registration is transactional and independent of PTT.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use thiscord_shared::audio::{AudioShortcutStatus, AudioShortcuts, AudioToggle};

#[derive(Default)]
pub struct ShortcutStateStore(Mutex<Bindings>);
#[derive(Default)]
struct Bindings {
    settings: AudioShortcuts,
    registered: Vec<Shortcut>,
    error: Option<String>,
}

fn parse(settings: &AudioShortcuts) -> Result<Vec<(Shortcut, AudioToggle)>, String> {
    let mut keys = Vec::new();
    let ptt: Shortcut = "Control+Shift+Space".parse().unwrap();
    for (value, action) in [
        (&settings.mute, AudioToggle::Mute),
        (&settings.deafen, AudioToggle::Deafen),
    ] {
        if value.len() > 128 {
            return Err("Shortcut is too long".into());
        }
        if value.trim().is_empty() {
            continue;
        }
        let key: Shortcut = value
            .parse()
            .map_err(|_| "Invalid shortcut. Use a combination such as Control+Shift+M.")?;
        if key == ptt {
            return Err("Ctrl+Shift+Space is reserved for push-to-talk".into());
        }
        if keys.iter().any(|(existing, _)| *existing == key) {
            return Err("Mute and deafen must use different shortcuts".into());
        }
        keys.push((key, action));
    }
    Ok(keys)
}

fn install(
    app: &tauri::AppHandle,
    state: &mut Bindings,
    settings: AudioShortcuts,
) -> Result<(), String> {
    let keys = parse(&settings)?;
    #[cfg(target_os = "linux")]
    if !keys.is_empty() && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return Err("Global shortcuts need an X11 session. Clear both shortcuts to disable them on Wayland.".into());
    }
    let mut added = Vec::new();
    for (key, _) in &keys {
        if state.registered.contains(key) {
            continue;
        }
        let down = Arc::new(AtomicBool::new(false));
        let result = app
            .global_shortcut()
            .on_shortcut(*key, move |app, key, event| {
                if event.state == ShortcutState::Released {
                    down.store(false, Ordering::Release);
                    return;
                }
                if down.swap(true, Ordering::AcqRel) {
                    return;
                }
                let app = app.clone();
                let key = *key;
                tauri::async_runtime::spawn(async move {
                    let action = app
                        .state::<ShortcutStateStore>()
                        .0
                        .lock()
                        .ok()
                        .and_then(|s| {
                            parse(&s.settings)
                                .ok()?
                                .into_iter()
                                .find(|(k, _)| *k == key)
                                .map(|(_, a)| a)
                        });
                    if let Some(action) = action
                        && let Err(error) =
                            crate::native_audio::audio_toggle(app.clone(), action).await
                        && let Ok(mut state) = app.state::<ShortcutStateStore>().0.lock()
                    {
                        state.error = Some(error);
                    }
                });
            });
        if result.is_err() {
            for key in added {
                let _ = app.global_shortcut().unregister(key);
            }
            return Err("Cannot register shortcut. Another app or an OS shortcut may be using it; try a different combination.".into());
        }
        added.push(*key);
    }
    let mut retained = Vec::new();
    for old in &state.registered {
        if !keys.iter().any(|(key, _)| key == old) {
            // Inactive callbacks check settings; retain failed unregisters so
            // subsequent edits can reuse/retry them without double registration.
            if app.global_shortcut().unregister(*old).is_err() {
                retained.push(*old);
            }
        }
    }
    state.registered = keys.into_iter().map(|(key, _)| key).collect();
    state.registered.extend(retained);
    state.settings = settings;
    state.error = None;
    Ok(())
}

pub fn start(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ShortcutStateStore>();
        let Ok(mut state) = state.0.lock() else {
            return;
        };
        let result = (|| {
            let path = app
                .path()
                .app_config_dir()
                .map_err(|_| "Settings directory unavailable")?
                .join("shortcuts.json");
            let settings = if path.exists() {
                if std::fs::metadata(&path)
                    .map_err(|_| "Cannot read shortcuts")?
                    .len()
                    > 4096
                {
                    return Err("Shortcut settings file is too large".into());
                }
                serde_json::from_slice(&std::fs::read(path).map_err(|_| "Cannot read shortcuts")?)
                    .map_err(|_| "Invalid shortcut settings")?
            } else {
                AudioShortcuts::default()
            };
            state.settings = settings.clone();
            install(&app, &mut state, settings)
        })();
        if let Err(error) = result {
            state.error = Some(error);
        }
    });
}

#[tauri::command]
pub async fn audio_shortcuts(app: tauri::AppHandle) -> Result<AudioShortcutStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ShortcutStateStore>();
        let state = state
            .0
            .lock()
            .map_err(|_| "Shortcut settings unavailable")?;
        Ok(AudioShortcutStatus {
            shortcuts: state.settings.clone(),
            error: state.error.clone(),
        })
    })
    .await
    .map_err(|_| "Shortcut task failed")?
}

#[tauri::command]
pub async fn audio_shortcuts_save(
    app: tauri::AppHandle,
    settings: AudioShortcuts,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ShortcutStateStore>();
        let mut state = state
            .0
            .lock()
            .map_err(|_| "Shortcut settings unavailable")?;
        let old = state.settings.clone();
        install(&app, &mut state, settings.clone())?;
        let result = (|| {
            let directory = app
                .path()
                .app_config_dir()
                .map_err(|_| "Settings directory unavailable")?;
            std::fs::create_dir_all(&directory).map_err(|_| "Cannot create settings directory")?;
            std::fs::write(
                directory.join("shortcuts.json.tmp"),
                serde_json::to_vec_pretty(&settings).map_err(|_| "Invalid shortcuts")?,
            )
            .map_err(|_| "Cannot save shortcuts")?;
            std::fs::rename(
                directory.join("shortcuts.json.tmp"),
                directory.join("shortcuts.json"),
            )
            .map_err(|_| "Cannot save shortcuts".to_string())
        })();
        if result.is_err()
            && let Err(error) = install(&app, &mut state, old)
        {
            state.error = Some(error);
        }
        result
    })
    .await
    .map_err(|_| "Shortcut task failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_alias_conflicts_ptt_and_disabled_bindings() {
        assert_eq!(parse(&AudioShortcuts::default()).unwrap().len(), 2);
        assert!(
            parse(&AudioShortcuts {
                mute: "Ctrl+Shift+M".into(),
                deafen: "Shift+Control+KeyM".into()
            })
            .is_err()
        );
        assert!(
            parse(&AudioShortcuts {
                mute: "Ctrl+Shift+Space".into(),
                deafen: String::new()
            })
            .is_err()
        );
        assert!(
            parse(&AudioShortcuts {
                mute: "not a key".into(),
                deafen: String::new()
            })
            .is_err()
        );
        assert!(
            parse(&AudioShortcuts {
                mute: String::new(),
                deafen: String::new()
            })
            .unwrap()
            .is_empty()
        );
    }
}
