//! Global self controls. Registration is transactional. The push-to-talk key
//! is validated here with the others but registered by `native_audio` only
//! while a push-to-talk call uses it.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use thiscord_shared::audio::{AudioShortcutStatus, AudioShortcuts, ShortcutAction};

#[derive(Default)]
pub struct ShortcutStateStore(Mutex<Bindings>);
#[derive(Default)]
struct Bindings {
    settings: AudioShortcuts,
    registered: Vec<Shortcut>,
    error: Option<String>,
}

/// The configured push-to-talk key; `None` when cleared (hold button only).
fn parse_push_to_talk(settings: &AudioShortcuts) -> Result<Option<Shortcut>, String> {
    let value = settings.push_to_talk.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > AudioShortcuts::MAX_LEN {
        return Err("Shortcut is too long".into());
    }
    let chord = thiscord_frontend::web_shortcuts::Chord::parse(value);
    if chord.as_ref().is_some_and(|c| c.blocks_typing()) {
        return Err(thiscord_frontend::web_shortcuts::TYPING_KEY.into());
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| "Invalid shortcut. Use a combination such as Control+Shift+M.".into())
}

/// The saved push-to-talk key for `native_audio::register_ptt`.
pub fn push_to_talk(app: &tauri::AppHandle) -> Result<Option<Shortcut>, String> {
    let state = app.state::<ShortcutStateStore>();
    let state = state
        .0
        .lock()
        .map_err(|_| "Shortcut settings unavailable")?;
    parse_push_to_talk(&state.settings)
}

fn parse(settings: &AudioShortcuts) -> Result<Vec<(Shortcut, ShortcutAction)>, String> {
    let mut keys = Vec::new();
    let ptt = parse_push_to_talk(settings)?;
    for (value, action) in settings.bindings() {
        if value.len() > AudioShortcuts::MAX_LEN {
            return Err("Shortcut is too long".into());
        }
        if value.trim().is_empty() {
            continue;
        }
        let key: Shortcut = value
            .parse()
            .map_err(|_| "Invalid shortcut. Use a combination such as Control+Shift+M.")?;
        if Some(key) == ptt || keys.iter().any(|(existing, _)| *existing == key) {
            return Err("Each action needs a different shortcut".into());
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
        return Err(
            "Global shortcuts need an X11 session. Clear all shortcuts to disable them on Wayland."
                .into(),
        );
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
                    let result = match action {
                        Some(ShortcutAction::Audio(action)) => {
                            crate::native_audio::audio_toggle(app.clone(), action).await
                        }
                        Some(ShortcutAction::Overlay(action)) => {
                            crate::native_overlay::action(app.clone(), action).await
                        }
                        None => Ok(()),
                    };
                    if let Err(error) = result
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
        // A call holding the push-to-talk key releases it first, so keys can be
        // swapped between push-to-talk and the toggles; it is re-registered below.
        let active = crate::native_audio::take_ptt_key(&app);
        let result = install(&app, &mut state, settings.clone()).and_then(|()| {
            write(&app, &settings).inspect_err(|_| {
                if let Err(error) = install(&app, &mut state, old.clone()) {
                    state.error = Some(error);
                }
            })
        });
        drop(state);
        if !active {
            // A call without a registered key (cleared or unavailable) picks up a new
            // key now; failures stay visible in the panel and the save stands.
            if result.is_ok()
                && settings.push_to_talk != old.push_to_talk
                && let Err(error) = crate::native_audio::refresh_ptt(&app)
                && let Ok(mut state) = app.state::<ShortcutStateStore>().0.lock()
            {
                state.error = Some(error);
            }
            return result;
        }
        if let Err(error) = result {
            // The previous shortcuts stayed installed; restore their push-to-talk key.
            let _ = crate::native_audio::refresh_ptt(&app);
            return Err(error);
        }
        if let Err(error) = crate::native_audio::refresh_ptt(&app) {
            // The new push-to-talk key is unavailable: keep the previous shortcuts.
            let state = app.state::<ShortcutStateStore>();
            let mut state = state
                .0
                .lock()
                .map_err(|_| "Shortcut settings unavailable")?;
            if let Err(error) =
                install(&app, &mut state, old.clone()).and_then(|()| write(&app, &old))
            {
                state.error = Some(error);
            }
            drop(state);
            let _ = crate::native_audio::refresh_ptt(&app);
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|_| "Shortcut task failed")?
}

fn write(app: &tauri::AppHandle, settings: &AudioShortcuts) -> Result<(), String> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?;
    std::fs::create_dir_all(&directory).map_err(|_| "Cannot create settings directory")?;
    std::fs::write(
        directory.join("shortcuts.json.tmp"),
        serde_json::to_vec_pretty(settings).map_err(|_| "Invalid shortcuts")?,
    )
    .map_err(|_| "Cannot save shortcuts")?;
    std::fs::rename(
        directory.join("shortcuts.json.tmp"),
        directory.join("shortcuts.json"),
    )
    .map_err(|_| "Cannot save shortcuts".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_bindings_register_as_desktop_shortcuts() {
        use thiscord_frontend::web_shortcuts::{Pressed, Recorded, record};
        let mut codes: Vec<String> = ('A'..='Z').map(|c| format!("Key{c}")).collect();
        codes.extend((0..=9).map(|d| format!("Digit{d}")));
        codes.extend((0..=9).map(|d| format!("Numpad{d}")));
        codes.extend((1..=24).map(|n| format!("F{n}")));
        codes.extend(
            [
                "Space",
                "Enter",
                "Tab",
                "Backspace",
                "Delete",
                "Insert",
                "Home",
                "End",
                "PageUp",
                "PageDown",
                "ArrowUp",
                "ArrowDown",
                "ArrowLeft",
                "ArrowRight",
                "Minus",
                "Equal",
                "Comma",
                "Period",
                "Slash",
                "Backslash",
                "Semicolon",
                "Quote",
                "Backquote",
                "BracketLeft",
                "BracketRight",
                "Pause",
                "ScrollLock",
                "PrintScreen",
                "NumpadAdd",
                "NumpadSubtract",
                "NumpadMultiply",
                "NumpadDivide",
                "NumpadDecimal",
                "NumpadEnter",
            ]
            .map(String::from),
        );
        for code in &codes {
            for (control, shift, alt, meta) in [
                (false, false, false, false),
                (true, true, false, false),
                (false, false, true, true),
            ] {
                let Recorded::Chord(text) = record(&Pressed {
                    code,
                    control,
                    shift,
                    alt,
                    meta,
                }) else {
                    panic!("{code} was not recorded");
                };
                let parsed: Result<Shortcut, _> = text.parse();
                assert!(parsed.is_ok(), "{text}");
            }
        }
    }
    #[test]
    fn validates_alias_conflicts_ptt_and_disabled_bindings() {
        assert_eq!(parse(&AudioShortcuts::default()).unwrap().len(), 2);
        assert!(
            parse(&AudioShortcuts {
                mute: "Ctrl+Shift+M".into(),
                deafen: "Shift+Control+KeyM".into(),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            parse(&AudioShortcuts {
                overlay_unlock: "Ctrl+Shift+Space".into(),
                ..Default::default()
            })
            .is_err()
        );
        // A moved push-to-talk key frees the old one and conflicts itself.
        assert!(
            parse(&AudioShortcuts {
                overlay_unlock: "Ctrl+Shift+Space".into(),
                push_to_talk: "F8".into(),
                ..Default::default()
            })
            .is_ok()
        );
        assert!(
            parse(&AudioShortcuts {
                push_to_talk: "Control+Shift+M".into(),
                ..Default::default()
            })
            .is_err()
        );
        for typing in ["V", "Space", "Shift+V"] {
            assert!(
                parse(&AudioShortcuts {
                    push_to_talk: typing.into(),
                    ..Default::default()
                })
                .is_err(),
                "{typing}"
            );
        }
        let cleared = AudioShortcuts {
            push_to_talk: String::new(),
            ..Default::default()
        };
        assert_eq!(parse_push_to_talk(&cleared).unwrap(), None);
        assert_eq!(
            parse_push_to_talk(&AudioShortcuts::default()).unwrap(),
            Some("Control+Shift+Space".parse().unwrap())
        );
        assert!(
            parse(&AudioShortcuts {
                overlay_corner: "not a key".into(),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            parse(&AudioShortcuts {
                mute: String::new(),
                deafen: String::new(),
                ..Default::default()
            })
            .unwrap()
            .is_empty()
        );
        // Overlay shortcuts join the same conflict check as mute and deafen.
        assert!(
            parse(&AudioShortcuts {
                overlay_visibility: "Control+Shift+M".into(),
                ..Default::default()
            })
            .is_err()
        );
        let all = parse(&AudioShortcuts {
            overlay_visibility: "Control+Shift+O".into(),
            overlay_corner: "Control+Shift+K".into(),
            overlay_unlock: "Control+Shift+L".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(all.len(), 5);
        assert!(all.iter().any(|(_, action)| *action
            == ShortcutAction::Overlay(thiscord_shared::overlay::OverlayAction::NextCorner)));
    }
}
