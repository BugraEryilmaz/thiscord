//! Control-only IPC. Device samples remain on native threads.
use std::sync::Mutex;
use tauri::{Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use thiscord_frontend::audio::{self, AudioEngine, Command};
use thiscord_shared::audio::*;

pub struct AudioState {
    pub engine: AudioEngine,
    file_lock: Mutex<()>,
}
impl Default for AudioState {
    fn default() -> Self {
        Self {
            engine: AudioEngine::new(),
            file_lock: Mutex::new(()),
        }
    }
}
const HOTKEY: &str = "Control+Shift+Space";
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
#[tauri::command]
pub async fn audio_load(app: tauri::AppHandle) -> Result<AudioSettings, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = app
            .path()
            .app_config_dir()
            .map_err(|_| "Settings directory unavailable")?
            .join("audio.json");
        if !path.exists() {
            return Ok(AudioSettings::default());
        }
        if std::fs::metadata(&path)
            .map_err(|_| "Cannot read audio settings")?
            .len()
            > 16384
        {
            return Err("Audio settings file is too large".into());
        }
        let settings: AudioSettings =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| "Cannot read audio settings")?)
                .map_err(|_| "Invalid audio settings file")?;
        settings.validate()?;
        Ok(settings)
    })
    .await
    .map_err(|_| "Settings task failed")?
}
#[tauri::command]
pub async fn audio_save(app: tauri::AppHandle, settings: AudioSettings) -> Result<(), String> {
    settings.validate()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AudioState>();
        let _guard = state.file_lock.lock().map_err(|_| "Settings unavailable")?;
        // Apply controls before disk I/O: a failed save must not leave a mic unmuted.
        state.engine.command(Command::Settings(settings.clone()))?;
        if let Ok(mut configuration) = app
            .state::<crate::native_voice::VoiceState>()
            .settings
            .lock()
        {
            if (configuration.mode != settings.mode
                || configuration.global_push_to_talk != settings.global_push_to_talk)
                && state.engine.command(Command::Peek)?.running
            {
                register_ptt(&app, &settings)?;
            }
            *configuration = settings.clone();
        }
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
#[tauri::command]
pub async fn audio_test(
    app: tauri::AppHandle,
    state: State<'_, AudioState>,
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
    let result = command(
        state.engine.clone(),
        Command::Start {
            settings,
            microphone,
        },
    )
    .await;
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
pub async fn audio_volume(
    state: State<'_, AudioState>,
    stream: usize,
    gain: f32,
) -> Result<AudioStatus, String> {
    command(state.engine.clone(), Command::Volume { stream, gain }).await
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
