mod leftpanel;
mod login;
mod lefticon;
mod create_server;
mod server;

use leptos::{context, logging::error, prelude::*, task::spawn_local};
use serde_wasm_bindgen::from_value;
use front_shared::LoginStatus;
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
        let is_logged_in = invoke("check_cookies", JsValue::UNDEFINED).await;
        match is_logged_in {
            Ok(value) => {
                let status: LoginStatus = from_value(value).unwrap();
                is_logged_in_signal.set(status);
            }
            Err(e) => {
                error!("Failed to check login status: {}", e.as_string().unwrap_or_else(|| "Unknown error".to_string()));
                is_logged_in_signal.set(LoginStatus::LoggedOut);
            }
        }
    });

    let active_server = RwSignal::new(None);

    view! {
        <main class=style::home_container>
            <leftpanel::Sidebar active_server=active_server />
            <Show when=move || !matches!(is_logged_in_signal.get(), LoginStatus::LoggedOut) fallback=move || view! { <login::Login /> }>
                <ServerComponent active_server=active_server />
            </Show>
        </main>
    }
}
