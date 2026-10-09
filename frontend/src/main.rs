#[cfg(target_arch = "wasm32")]
mod account_client;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod overlay;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    if web_sys::window()
        .is_some_and(|w| w.location().hash().ok().as_deref() == Some("#voice-overlay"))
    {
        leptos::mount::mount_to_body(overlay::VoiceOverlay);
    } else {
        leptos::mount::mount_to_body(app::App);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Use `trunk serve` for the UI or `cargo tauri dev` for the desktop app.");
}
