#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod native_account;
mod native_audio;
mod native_update;
mod native_voice;

fn main() {
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
        .on_window_event(|window, event| {
            use tauri::Manager;
            let engine = &window.state::<native_audio::AudioState>().engine;
            match event {
                tauri::WindowEvent::Destroyed => {
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
            native_audio::audio_save,
            native_audio::audio_test,
            native_audio::audio_stop,
            native_audio::audio_status,
            native_audio::audio_volume,
            native_audio::audio_pressed,
            native_audio::audio_webrtc_probe,
            native_voice::voice_join,
            native_voice::voice_leave,
            native_voice::voice_status,
            native_update::update_status,
            native_update::update_check,
            native_update::update_install
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Thiscord");
}
