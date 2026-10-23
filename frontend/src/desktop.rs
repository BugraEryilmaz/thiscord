#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod native_account;
mod native_audio;
mod native_bots;
mod native_camera;
mod native_download;
mod native_links;
mod native_notifications;
mod native_overlay;
mod native_performance;
mod native_remote_audio;
mod native_screen;
mod native_shortcuts;
mod native_signaling;
mod native_update;
mod native_voice;
mod native_window;
#[cfg(target_os = "windows")]
mod system_audio;

fn main() {
    // Run from a Terminal to diagnose the exact installed native network path.
    // No WebView, microphone, account token or TURN allocation is involved.
    if std::env::args().any(|arg| arg == "--check-voice-connection") {
        let runtime = tokio::runtime::Runtime::new().expect("create diagnostic runtime");
        let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
        println!(
            "Thiscord {} native voice connection check",
            env!("CARGO_PKG_VERSION")
        );
        let result = runtime.block_on(native_signaling::connect(base));
        let code = match result {
            Ok(_) => {
                println!("Voice TLS/WebSocket connection succeeded.");
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        };
        std::process::exit(code);
    }
    tauri::Builder::default()
        // Register first so a second launch exits before initializing other plugins.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            native_window::restore(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(native_update::UpdateState::default())
        .manage(native_notifications::NotificationState::default())
        .manage(native_overlay::OverlayState::default())
        .manage(native_shortcuts::ShortcutStateStore::default())
        .setup(|app| {
            use tauri::Manager;
            native_download::create_main_window(app)?;
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                app.state::<native_camera::CameraState>().attach(window);
            }
            if native_window::start(app.handle()).is_err() {
                eprintln!("System tray unavailable; close-to-tray is disabled");
            }
            let directory = app.path().app_local_data_dir()?.join("diagnostics");
            let preference = app.path().app_config_dir()?.join("diagnostics.json");
            if let Err(error) =
                thiscord_frontend::audio::diagnostics::initialize(directory, preference)
            {
                eprintln!("{error}");
            }
            app.state::<native_audio::AudioState>()
                .engine
                .diagnostic_snapshot();
            // An unsupported compositor must not prevent chat/voice from starting.
            if native_overlay::start(app.handle()).is_err() {
                eprintln!("Voice overlay unavailable on this display server");
            }
            native_update::start(app.handle());
            native_shortcuts::start(app.handle());
            Ok(())
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(native_audio::AudioState::default())
        .manage(native_voice::VoiceState::default())
        .manage(native_screen::ScreenState::default())
        .manage(native_camera::CameraState::default())
        .on_window_event(|window, event| {
            use tauri::Manager;
            if window.label() != "main" {
                return;
            }
            let engine = &window.state::<native_audio::AudioState>().engine;
            match event {
                tauri::WindowEvent::CloseRequested { api, .. }
                    if native_window::close_to_tray(window.app_handle()) =>
                {
                    api.prevent_close();
                    engine.notify(thiscord_frontend::audio::Command::Pressed(false));
                    // Linux can create an indicator without a tray host displaying it.
                    // Keep a taskbar recovery path on those desktops.
                    #[cfg(target_os = "linux")]
                    let _ = window.minimize();
                    #[cfg(not(target_os = "linux"))]
                    if window.hide().is_err() {
                        let _ = window.minimize();
                    }
                }
                tauri::WindowEvent::Destroyed => {
                    thiscord_frontend::audio::diagnostics::event(
                        "window_destroyed",
                        serde_json::json!({}),
                    );
                    window.state::<native_screen::ScreenState>().clear();
                    window.state::<native_camera::CameraState>().clear();
                    engine.notify(thiscord_frontend::audio::Command::Stop);
                    // Auxiliary windows must not keep a normally closed app alive.
                    window.app_handle().exit(0);
                }
                tauri::WindowEvent::Focused(false) => {
                    thiscord_frontend::audio::diagnostics::event(
                        "window_focus",
                        serde_json::json!({"focused":false}),
                    );
                    engine.notify(thiscord_frontend::audio::Command::Pressed(false))
                }
                tauri::WindowEvent::Focused(true) => {
                    thiscord_frontend::audio::diagnostics::event(
                        "window_focus",
                        serde_json::json!({"focused":true}),
                    );
                }
                _ => {}
            }
        })
        .manage(native_account::CallbackState::default())
        .invoke_handler(tauri::generate_handler![
            native_performance::performance_report,
            native_window::window_close_to_tray,
            native_window::window_set_close_to_tray,
            native_account::load_session,
            native_account::save_session,
            native_bots::save_bot_credential,
            native_account::clear_session,
            native_account::prepare_google,
            native_account::open_google,
            native_account::cancel_google,
            native_links::open_external_url,
            native_audio::audio_devices,
            native_audio::audio_load,
            native_audio::audio_current,
            native_audio::audio_save,
            native_audio::audio_restrict,
            native_audio::audio_test,
            native_audio::audio_stop,
            native_audio::audio_status,
            native_audio::audio_debug_start,
            native_audio::audio_debug_stop,
            native_audio::audio_debug_folder,
            native_audio::audio_diagnostics_folder,
            native_audio::audio_diagnostics_enabled,
            native_audio::audio_diagnostics_enable,
            native_audio::audio_volume,
            native_audio::audio_pressed,
            native_audio::audio_webrtc_probe,
            native_camera::camera_prepare,
            native_camera::camera_open,
            native_camera::camera_keepalive,
            native_camera::camera_stop,
            native_screen::screen_status,
            native_screen::screen_view_diagnostics,
            native_screen::screen_view_keyframe,
            native_screen::screen_view_open,
            native_screen::screen_view_keepalive,
            native_screen::screen_sources,
            native_screen::screen_start,
            native_screen::screen_stop,
            native_voice::voice_join,
            native_voice::voice_leave,
            native_voice::voice_status,
            native_audio::audio_toggle,
            native_shortcuts::audio_shortcuts,
            native_shortcuts::audio_shortcuts_save,
            native_overlay::overlay_toggle_audio,
            native_overlay::overlay_snapshot,
            native_overlay::overlay_enabled,
            native_overlay::overlay_enable,
            native_update::update_status,
            native_notifications::notification_show,
            native_notifications::notification_clear,
            native_notifications::notification_remove,
            native_notifications::notification_sync_calls,
            native_notifications::notification_snoozed,
            native_notifications::notification_sound,
            native_notifications::notification_snapshot,
            native_notifications::notification_resize,
            native_notifications::notification_action,
            native_update::update_check,
            native_update::update_install
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Thiscord")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                native_window::restore(app);
            }
            if matches!(event, tauri::RunEvent::Exit) {
                native_audio::finish_recording_on_exit(app);
            }
        });
}
