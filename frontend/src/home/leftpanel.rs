use leptos::{context, logging::log, prelude::*, task::spawn_local};
use shared::Server;
use wasm_bindgen::JsValue;

use crate::{app::LoggedInSignal, utils::invoke};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

#[component]
pub fn Sidebar() -> impl IntoView {
    let (servers, set_servers) = signal(vec![]); 
    let is_logged_in_signal =
        context::use_context::<LoggedInSignal>().expect("SessionCookie context not found");

    Effect::new(move || {
        if !is_logged_in_signal.get() {
            set_servers.set(vec![]); // Clear servers for not logged-in users
        } else {
            spawn_local(async move {
                let servers = invoke("get_servers", JsValue::null()).await;
                if let Err(e) = servers {
                    log!("Failed to fetch servers: {:?}", e);
                    return;
                }
                let servers = servers.unwrap();
                log!("Fetched servers: {:?}", servers);
                let servers: Vec<Server> = serde_wasm_bindgen::from_value(servers).unwrap_or_default();
                set_servers.set(servers);
            });
            log!("User is logged in");
        }
    });
    view! {
        <div class=style::sidebar>
            <ol>
                <For
                    each=move || servers.get()
                    key=|server| server.id
                    children=move |server| {
                        view! {
                            <li>
                                <span>{server.name}</span>
                                <span>{server.image}</span>
                            </li>
                        }
                    }
                />
            </ol>
        </div>
    }
}
