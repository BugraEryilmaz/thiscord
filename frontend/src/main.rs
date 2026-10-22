#[cfg(target_arch = "wasm32")]
mod account_client;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod audio_control;
#[cfg(target_arch = "wasm32")]
mod avatar;
#[cfg(target_arch = "wasm32")]
mod i18n;
#[cfg(target_arch = "wasm32")]
mod notification_banner;
#[cfg(target_arch = "wasm32")]
mod overlay;
#[cfg(target_arch = "wasm32")]
mod performance;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    if web_sys::window()
        .is_some_and(|w| w.location().hash().ok().as_deref() == Some("#voice-overlay"))
    {
        leptos::mount::mount_to_body(|| {
            i18n::initialize();
            overlay::VoiceOverlay()
        });
    } else if web_sys::window()
        .is_some_and(|w| w.location().hash().ok().as_deref() == Some("#notifications"))
    {
        leptos::mount::mount_to_body(|| {
            i18n::initialize();
            notification_banner::DesktopBanners()
        });
    } else {
        leptos::mount::mount_to_body(|| {
            i18n::initialize();
            app::App()
        });
        // mount_to_body initializes Leptos's local task executor.
        performance::install();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Use `trunk serve` for the UI or `cargo tauri dev` for the desktop app.");
}
