#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod native_account;
mod native_audio;
mod native_screen;
mod native_signaling;
mod native_update;
mod native_voice;
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(native_update::UpdateState::default())
        .setup(|app| {
            native_update::start(app.handle());
            Ok(())
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(native_audio::AudioState::default())
        .manage(native_voice::VoiceState::default())
        .manage(native_screen::ScreenState::default())
        .register_uri_scheme_protocol("screen", |ctx, request| {
            native_screen::protocol(ctx.app_handle(), request)
        })
        .on_window_event(|window, event| {
            use tauri::Manager;
            let engine = &window.state::<native_audio::AudioState>().engine;
            match event {
                tauri::WindowEvent::Destroyed => {
                    window.state::<native_screen::ScreenState>().clear();
                    engine.notify(thiscord_frontend::audio::Command::Stop)
                }
                tauri::WindowEvent::Focused(false) => {
                    engine.notify(thiscord_frontend::audio::Command::Pressed(false))
                }
                _ => {}
            }
        })
        .manage(native_account::CallbackState::default())
        .invoke_handler(tauri::generate_handler![
            native_account::load_session,
            native_account::save_session,
            native_account::clear_session,
            native_account::prepare_google,
            native_account::open_google,
            native_account::cancel_google,
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
            native_audio::audio_volume,
            native_audio::audio_pressed,
            native_audio::audio_webrtc_probe,
            native_screen::screen_status,
            native_screen::screen_frame_state,
            native_screen::screen_sources,
            native_screen::screen_start,
            native_screen::screen_stop,
            native_voice::voice_join,
            native_voice::voice_leave,
            native_voice::voice_status,
            native_update::update_status,
            native_update::update_check,
            native_update::update_install
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Thiscord")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                native_audio::finish_recording_on_exit(app);
            }
        });
}
