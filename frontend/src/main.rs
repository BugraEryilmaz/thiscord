#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod account_client;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod app;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod audio_control;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod avatar;
#[cfg(target_arch = "wasm32")]
mod brand;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod i18n;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod notification_banner;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod overlay;
#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
mod performance;
#[cfg(all(target_arch = "wasm32", feature = "welcome"))]
mod welcome;

#[cfg(all(target_arch = "wasm32", feature = "welcome"))]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(welcome::Welcome);
}

#[cfg(all(target_arch = "wasm32", not(feature = "welcome")))]
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
