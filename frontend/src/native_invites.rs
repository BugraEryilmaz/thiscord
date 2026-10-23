use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tauri_plugin_deep_link::DeepLinkExt;

#[derive(Default)]
pub struct InviteState(Mutex<Option<String>>);

fn receive(app: &tauri::AppHandle, urls: Vec<url::Url>) {
    let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
    for url in urls {
        if let Some(code) = thiscord_frontend::invites::desktop_code(url.as_str(), base) {
            // One pending invitation, kept only in memory until the UI consumes it.
            if let Ok(mut pending) = app.state::<InviteState>().0.lock() {
                *pending = Some(code);
            }
            // Notify only the main window, without exposing the capability in events.
            let _ = app.emit_to("main", "invite-pending", ());
            crate::native_window::restore(app);
        }
    }
}

pub fn start(app: &tauri::AppHandle) -> tauri_plugin_deep_link::Result<()> {
    let handle = app.clone();
    app.deep_link()
        .on_open_url(move |event| receive(&handle, event.urls()));
    if let Some(urls) = app.deep_link().get_current()? {
        receive(app, urls);
    }
    // AppImages need registration at their current filesystem location.
    #[cfg(target_os = "linux")]
    app.deep_link().register_all()?;
    Ok(())
}

#[tauri::command]
pub fn take_pending_invite(state: tauri::State<'_, InviteState>) -> Option<String> {
    state.0.lock().ok()?.take()
}
