use leptos::{context, logging::log, prelude::*, task::spawn_local};
use front_shared::{URL};
use shared::models::Server;
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    app::LoggedInSignal,
    home::create_server::CreateServerPopup,
    utils::{invoke},
};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

#[component]
pub fn Sidebar(active_server: RwSignal<Option<Server>>) -> impl IntoView {
    let (servers, set_servers) = signal(vec![]);
    let is_logged_in_signal =
        context::use_context::<LoggedInSignal>().expect("SessionCookie context not found");
    let (create_server_popup, set_create_server_popup) = signal(false);

    Effect::new(move || {
        if !is_logged_in_signal.get() {
            set_servers.set(vec![]); // Clear servers for not logged-in users
            active_server.set(None);
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
                log!("Parsed servers: {:?}", servers);
                active_server.update(|old| {
                    if old.is_none() {
                        *old = servers.first().cloned()
                    }
                });
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
                        view! {
                            <LeftIcon
                                img_url=format!("https://{}/{}", URL, server.image_url.clone().unwrap_or("/static/server/NOTFOUND.png".to_string()))
                                name=server.name.clone()
                                onclick=move || {
                                    log!("Setting active server: {:?}", server);
                                    active_server.set(Some(server.clone()));
                                }
                            />
                        }
                    }
                />
                <LeftIcon
                    img_url="/public/new_server.svg".to_string()
                    name="Add Server".to_string()
                    onclick=move || {
                        set_create_server_popup.set(true);
                    }
                />
            </ul>
            <Show when=move || create_server_popup.get()>
                <div class=style::overlay on:click=move |_| set_create_server_popup.set(false) />
                <CreateServerPopup on_create=move || {
                    set_create_server_popup.set(false);
                    is_logged_in_signal.update(|_| {});
                } />
            </Show>
        </div>
    }
}

#[component]
pub fn LeftIcon(img_url: String, name: String, mut onclick: impl FnMut() -> () + 'static) -> impl IntoView {
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
            on:click=move |_| {
                onclick();
            }
        >
            <img
                src=img_url
                class=style::server_list_icon
                on:error=move |event: web_sys::ErrorEvent| {
                    log!("Failed to load server icon: {:?}", event);
                    let target = event.target().unwrap();
                    if let Some(img) = target
                        .dyn_ref::<web_sys::HtmlImageElement>()
                    {
                        img.set_src("/public/leptos.svg");
                    }
                }
            />
            <span class=style::server_list_name style:top=top_signal>
                {name}
            </span>
        </li>
    }
}
