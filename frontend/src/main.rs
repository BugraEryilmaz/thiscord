#[cfg(target_arch = "wasm32")]
mod account_client;
#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Use `trunk serve` for the UI or `cargo tauri dev` for the desktop app.");
}
