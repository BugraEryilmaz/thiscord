mod leftpanel;
mod login;
mod lefticon;
mod create_server;
mod server;

use leptos::{context, logging::error, prelude::*, task::spawn_local};
use serde_wasm_bindgen::from_value;
use shared::LoginStatus;
use wasm_bindgen::JsValue;

use crate::{app::LoggedInSignal, home::server::ServerComponent, utils::{invoke}};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home/home.css"
);

#[component]
pub fn Home() -> impl IntoView {
    let is_logged_in_signal =
        context::use_context::<LoggedInSignal>().expect("SessionCookie context not found");

    // Check if the user is logged in by checking the session cookie
    spawn_local(async move {
        let is_logged_in = invoke("check_cookies", JsValue::UNDEFINED).await.unwrap_or_else(|_| JsValue::from(false));
        if let Ok(is_logged_in) = from_value::<bool>(is_logged_in) {
            is_logged_in_signal.set(is_logged_in.into());
        } else {
            error!("Failed to check cookies");
        }
    });

    let (active_server, set_active_server) = signal(None);

    view! {
        <main class=style::home_container>
            <leftpanel::Sidebar set_active_server=set_active_server />
            <Show when=move || is_logged_in_signal.get() == LoginStatus::LoggedIn fallback=move || view! { <login::Login /> }>
                <ServerComponent active_server=active_server />
            </Show>
        </main>
    }
}
