mod app;
pub mod utils;
mod home;
mod server;

use app::*;
use leptos::prelude::*;


fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| {
        view! { <App /> }
    })
}
