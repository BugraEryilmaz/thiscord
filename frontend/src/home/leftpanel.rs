use leptos::{context, logging::log, prelude::*, task::spawn_local};
use shared::{Server, URL};
use wasm_bindgen::{JsCast, JsValue};

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
                let servers: Vec<Server> =
                    serde_wasm_bindgen::from_value(servers).unwrap_or_default();
                set_servers.set(servers);
            });
            log!("User is logged in");
        }
    });
    view! {
        <div class=style::sidebar>
            <ul class=style::server_list>
                <For
                    each=move || servers.get()
                    key=|server| server.id
                    children=move |server| {
                        let parent = NodeRef::new();
                        let (top_signal, set_top_signal) = signal("0px".to_string());
                        view! {
                            <li
                                class=style::server_list_item
                                node_ref=parent
                                on:mouseover=move |_| {
                                    if let Some(parent) = parent.get() {
                                        let top = parent.get_bounding_client_rect().top();
                                        set_top_signal.set(format!("{}px", top + 32.0));
                                    }
                                }
                            >
                                <img
                                    src=format!("{}/{}", URL, server.image_url)
                                    class=style::server_list_icon
                                    on:error=move |event: web_sys::ErrorEvent| {
                                        log!("Failed to load server icon: {:?}", event);
                                        // Optionally set a default icon or handle the error
                                        let target = event.target().unwrap();
                                        if let Some(img) = target.dyn_ref::<web_sys::HtmlImageElement>() {
                                            img.set_src("/public/leptos.svg");
                                        }
                                    }
                                />
                                <span class=style::server_list_name style:top=top_signal>
                                    {server.name.clone()}
                                </span>
                            </li>
                        }
                    }
                />
            </ul>
        </div>
    }
}
