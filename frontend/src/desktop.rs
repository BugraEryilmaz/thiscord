#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod native_account;

fn main() {
    tauri::Builder::default()
        .manage(native_account::CallbackState::default())
        .invoke_handler(tauri::generate_handler![
            native_account::load_session,
            native_account::save_session,
            native_account::clear_session,
            native_account::prepare_google,
            native_account::open_google,
            native_account::cancel_google
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Thiscord");
}
