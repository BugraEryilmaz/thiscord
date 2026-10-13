//! Control-only IPC. Device samples remain on native threads.
use std::sync::Mutex;
use tauri::{Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use thiscord_frontend::audio::{self, AudioEngine, Command};
use thiscord_shared::audio::*;

pub struct AudioState {
    pub engine: AudioEngine,
    file_lock: Mutex<Option<AudioSettings>>,
}
impl Default for AudioState {
    fn default() -> Self {
        Self {
            engine: AudioEngine::new(),
            file_lock: Mutex::new(None),
        }
    }
}
const HOTKEY: &str = "Control+Shift+Space";
#[tauri::command]
pub async fn audio_diagnostics_enabled() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(audio::diagnostics::enabled)
        .await
        .map_err(|_| "Diagnostics settings task failed")?
}
#[tauri::command]
pub async fn audio_diagnostics_enable(enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || audio::diagnostics::set_enabled(enabled))
        .await
        .map_err(|_| "Diagnostics settings task failed")?
}
#[tauri::command]
pub async fn audio_diagnostics_folder(state: State<'_, AudioState>) -> Result<(), String> {
    // Does not send a worker command: usable even when that worker is hung.
    state.engine.diagnostic_snapshot();
    let directory = audio::diagnostics::directory()?;
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&directory).map_err(|_| "Cannot create diagnostics folder")?;
        open::that(directory).map_err(|_| "Cannot open diagnostics folder".to_string())
    })
    .await
    .map_err(|_| "Cannot open diagnostics folder")?
}
async fn command(engine: AudioEngine, command: Command) -> Result<AudioStatus, String> {
    tauri::async_runtime::spawn_blocking(move || engine.command(command))
        .await
        .map_err(|_| "Audio worker stopped")?
}
#[tauri::command]
pub async fn audio_devices() -> Result<Vec<AudioDevice>, String> {
    tauri::async_runtime::spawn_blocking(audio::devices)
        .await
        .map_err(|_| "Device scan failed")?
}
fn load_settings(
    app: &tauri::AppHandle,
    cached: &mut Option<AudioSettings>,
) -> Result<AudioSettings, String> {
    if let Some(settings) = cached {
        return Ok(settings.clone());
    }
    let path = app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?
        .join("audio.json");
    let settings = if path.exists() {
        if std::fs::metadata(&path)
            .map_err(|_| "Cannot read audio settings")?
            .len()
            > 16384
        {
            return Err("Audio settings file is too large".into());
        }
        serde_json::from_slice::<AudioSettings>(
            &std::fs::read(path).map_err(|_| "Cannot read audio settings")?,
        )
        .map_err(|_| "Invalid audio settings file")?
    } else {
        AudioSettings::default()
    };
    settings.validate()?;
    *cached = Some(settings.clone());
    Ok(settings)
}
#[tauri::command]
pub async fn audio_load(app: tauri::AppHandle) -> Result<AudioSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AudioState>();
        let mut cached = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
        load_settings(&app, &mut cached)
    })
    .await
    .map_err(|_| "Settings task failed")?
}
#[tauri::command]
pub async fn audio_current(app: tauri::AppHandle) -> Result<AudioSettings, String> {
    audio_load(app).await
}

/// All entry points toggle the latest native state, never a WebView snapshot.
#[tauri::command]
pub async fn audio_toggle(app: tauri::AppHandle, action: AudioToggle) -> Result<(), String> {
    // Tighten gates immediately even while a device/model save holds file_lock.
    // Only the serialized change below can relax them or persist preferences.
    if let Ok(mut settings) = app
        .state::<crate::native_voice::VoiceState>()
        .settings
        .lock()
        .map(|s| s.clone())
    {
        action.apply(&mut settings);
        audio_restrict(app.clone(), app.state(), settings)?;
    }
    change_settings(app, None, Some(action)).await
}
#[tauri::command]
pub fn audio_restrict(
    app: tauri::AppHandle,
    state: State<'_, AudioState>,
    settings: AudioSettings,
) -> Result<(), String> {
    settings.validate()?;
    state.engine.restrict(&settings);
    if settings.deafened {
        crate::native_screen::screen_stop(app.state::<crate::native_screen::ScreenState>());
        app.state::<crate::native_screen::ScreenState>().roster(&[]);
    }
    Ok(())
}
#[tauri::command]
pub async fn audio_save(app: tauri::AppHandle, settings: AudioSettings) -> Result<(), String> {
    settings.validate()?;
    change_settings(app, Some(settings), None).await
}
async fn change_settings(
    app: tauri::AppHandle,
    requested: Option<AudioSettings>,
    action: Option<AudioToggle>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AudioState>();
        let mut cached = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
        let previous = load_settings(&app, &mut cached)?;
        let settings = merge_settings(&previous, requested, action);
        state.engine.restrict(&settings);
        if settings.deafened {
            crate::native_screen::screen_stop(app.state::<crate::native_screen::ScreenState>());
            app.state::<crate::native_screen::ScreenState>().roster(&[]);
        }
        let voice = app.state::<crate::native_voice::VoiceState>();
        let shortcut_changed = (previous.mode != settings.mode
            || previous.global_push_to_talk != settings.global_push_to_talk)
            && state.engine.command(Command::Peek)?.running;
        if shortcut_changed && let Err(error) = register_ptt(&app, &settings) {
            let _ = register_ptt(&app, &previous);
            return Err(error);
        }
        // Apply controls before disk I/O: a failed save must not leave a mic unmuted.
        if let Err(error) = state.engine.command(Command::Settings(settings.clone())) {
            // A failed command must never relax a previously active restriction.
            // Keep any newly applied safety gate visible to subsequent saves.
            let mut safe = previous.clone();
            safe.muted |= settings.muted;
            safe.deafened |= settings.deafened;
            state.engine.restrict(&safe);
            if let Ok(mut configuration) = voice.settings.lock() {
                configuration.muted = safe.muted;
                configuration.deafened = safe.deafened;
            }
            *cached = Some(safe);
            if shortcut_changed {
                if state.engine.command(Command::Peek).is_ok_and(|s| s.running) {
                    // Do not replace a newer session's shortcut after cancellation.
                    if voice.settings.lock().is_ok_and(|current| {
                        current.mode == previous.mode
                            && current.global_push_to_talk == previous.global_push_to_talk
                    }) {
                        let _ = register_ptt(&app, &previous);
                    }
                } else {
                    unregister_ptt(&app);
                }
            }
            return Err(error);
        }
        if let Ok(mut configuration) = voice.settings.lock() {
            *configuration = settings.clone();
        }
        *cached = Some(settings.clone());
        let directory = app
            .path()
            .app_config_dir()
            .map_err(|_| "Settings directory unavailable")?;
        std::fs::create_dir_all(&directory).map_err(|_| "Cannot create settings directory")?;
        let data = serde_json::to_vec_pretty(&settings).map_err(|_| "Invalid settings")?;
        std::fs::write(directory.join("audio.json.tmp"), data)
            .map_err(|_| "Cannot save audio settings")?;
        std::fs::rename(
            directory.join("audio.json.tmp"),
            directory.join("audio.json"),
        )
        .map_err(|_| "Cannot save audio settings")?;
        Ok(())
    })
    .await
    .map_err(|_| "Settings task failed")?
}

fn merge_settings(
    previous: &AudioSettings,
    requested: Option<AudioSettings>,
    action: Option<AudioToggle>,
) -> AudioSettings {
    let mut settings = requested.unwrap_or_else(|| previous.clone());
    // Device/processing saves cannot undo controls from the overlay or hotkeys.
    settings.muted = previous.muted;
    settings.deafened = previous.deafened;
    if let Some(action) = action {
        action.apply(&mut settings);
    }
    settings
}

#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn stale_device_saves_cannot_undo_toggles_and_toggles_preserve_devices() {
        let before = AudioSettings::default();
        let muted = merge_settings(&before, None, Some(AudioToggle::Mute));
        let deafened = merge_settings(&muted, None, Some(AudioToggle::Deafen));
        let mut stale = before;
        stale.input = Some("new microphone".into());
        let saved = merge_settings(&deafened, Some(stale), None);
        assert!(saved.muted && saved.deafened);
        assert_eq!(saved.input.as_deref(), Some("new microphone"));
        let restored = merge_settings(&saved, None, Some(AudioToggle::Deafen));
        assert!(restored.muted && !restored.deafened);
        assert_eq!(restored.input, saved.input);
    }
}
#[tauri::command]
pub async fn audio_test(
    app: tauri::AppHandle,
    settings: AudioSettings,
    microphone: bool,
) -> Result<AudioStatus, String> {
    settings.validate()?;
    let update_state = app.state::<crate::native_update::UpdateState>();
    let _admission = update_state
        .voice_admission
        .try_lock()
        .map_err(|_| "Wait for the update or voice connection to finish")?;
    if microphone {
        register_ptt(&app, &settings)?;
    }
    let start_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = start_app.state::<AudioState>();
        let mut cached = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
        let current = load_settings(&start_app, &mut cached)?;
        let mut settings = settings;
        settings.muted = current.muted;
        settings.deafened = current.deafened;
        state.engine.command(Command::Start {
            settings,
            microphone,
        })
    })
    .await
    .map_err(|_| "Audio worker stopped")?;
    if result.is_err() {
        let _ = app.global_shortcut().unregister(HOTKEY);
    }
    result
}
#[tauri::command]
pub async fn audio_stop(
    app: tauri::AppHandle,
    state: State<'_, AudioState>,
) -> Result<AudioStatus, String> {
    let _ = app.global_shortcut().unregister(HOTKEY);
    command(state.engine.clone(), Command::Stop).await
}
#[tauri::command]
pub async fn audio_status(
    app: tauri::AppHandle,
    state: State<'_, AudioState>,
) -> Result<AudioStatus, String> {
    let result = command(state.engine.clone(), Command::Status).await?;
    if !result.running
        && crate::native_voice::voice_status(app.state()).is_ok_and(|s| s.channel_id.is_none())
    {
        let _ = app.global_shortcut().unregister(HOTKEY);
    }
    Ok(result)
}
#[tauri::command]
pub async fn audio_debug_start(
    app: tauri::AppHandle,
    state: State<'_, AudioState>,
) -> Result<AudioStatus, String> {
    let parent = app
        .path()
        .app_local_data_dir()
        .map_err(|_| "Recording directory unavailable")?
        .join("audio-debug");
    command(state.engine.clone(), Command::DebugStart(parent)).await
}
#[tauri::command]
pub async fn audio_debug_stop(state: State<'_, AudioState>) -> Result<AudioStatus, String> {
    command(state.engine.clone(), Command::DebugStop).await
}
#[tauri::command]
pub async fn audio_debug_folder(state: State<'_, AudioState>) -> Result<(), String> {
    let status = command(state.engine.clone(), Command::Peek).await?;
    let recording = status.recording.ok_or("No diagnostic recording yet")?;
    if recording.active || recording.saving {
        return Err("Stop recording and wait for files to finish saving".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        open::that(recording.directory).map_err(|_| "Cannot open recording folder".to_string())
    })
    .await
    .map_err(|_| "Cannot open recording folder")?
}

/// Graceful shutdown gives the independent writer time to finalize WAV headers.
/// The writer also checkpoints headers during recording for interrupted exits.
pub fn finish_recording_on_exit(app: &tauri::AppHandle) {
    let engine = &app.state::<AudioState>().engine;
    engine.notify(Command::Stop);
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(3) {
        if let Ok(status) = engine.command(Command::Peek)
            && status.recording.is_none_or(|r| !r.active && !r.saving)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
pub fn start_voice(
    app: &tauri::AppHandle,
    guild: thiscord_shared::GuildId,
    connection: audio::connection::Connection,
    settings: AudioSettings,
    outgoing: tokio::sync::mpsc::Sender<audio::outgoing::Packet>,
    microphone: bool,
) -> Result<AudioStatus, String> {
    let state = app.state::<AudioState>();
    let mut cached = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
    let current = load_settings(app, &mut cached)?;
    let mut settings = settings;
    settings.muted = current.muted;
    settings.deafened = current.deafened;
    *app.state::<crate::native_voice::VoiceState>()
        .settings
        .lock()
        .map_err(|_| "Voice settings unavailable")? = settings.clone();
    let path = app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?
        .join("speaker-volumes.json");
    let volumes = audio::volumes::Store::load(&path)?.profile(guild);
    state.engine.command(Command::VoiceStart {
        volumes,
        connection,
        settings,
        outgoing,
        microphone,
    })
}
#[tauri::command]
pub async fn audio_volume(
    app: tauri::AppHandle,
    stream: usize,
    gain: f32,
    target: Option<SpeakerVolumeTarget>,
) -> Result<AudioStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AudioState>();
        let _guard = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
        let path = app
            .path()
            .app_config_dir()
            .map_err(|_| "Settings directory unavailable")?
            .join("speaker-volumes.json");
        let mut store = if target.is_some() {
            Some(audio::volumes::Store::load(&path)?)
        } else {
            None
        };
        let status = state.engine.command(Command::Volume {
            stream,
            gain,
            target,
        })?;
        if let (Some(store), Some(target)) = (&mut store, target) {
            store.save(&path, target, gain)?;
        }
        Ok(status)
    })
    .await
    .map_err(|_| "Settings task failed")?
}
#[tauri::command]
pub async fn audio_pressed(
    state: State<'_, AudioState>,
    pressed: bool,
) -> Result<AudioStatus, String> {
    command(state.engine.clone(), Command::Pressed(pressed)).await
}
#[tauri::command]
pub async fn audio_webrtc_probe() -> Result<String, String> {
    static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if BUSY.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return Err("A WebRTC test is already running".into());
    }
    let result = audio::transport::probe().await;
    BUSY.store(false, std::sync::atomic::Ordering::Release);
    result
}

pub fn unregister_ptt(app: &tauri::AppHandle) {
    let _ = app.global_shortcut().unregister(HOTKEY);
    app.state::<AudioState>()
        .engine
        .notify(Command::Pressed(false));
}
pub fn register_ptt(app: &tauri::AppHandle, settings: &AudioSettings) -> Result<(), String> {
    unregister_ptt(app);
    if settings.global_push_to_talk && settings.mode == TransmitMode::PushToTalk {
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return Err(
                "Global push-to-talk needs an X11 session. Use the in-app hold button on Wayland."
                    .into(),
            );
        }
        app.global_shortcut().on_shortcut(HOTKEY,|app,_,event|{app.state::<AudioState>().engine.notify(Command::Pressed(event.state==ShortcutState::Pressed));}).map_err(|_|"Cannot register Ctrl+Shift+Space. Check OS shortcut permissions or another app using it.")?;
    }
    Ok(())
}
