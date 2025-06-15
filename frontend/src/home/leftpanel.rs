use leptos::{context, logging::log, prelude::*, task::spawn_local};
use shared::{Server, URL};
use wasm_bindgen::{JsCast, JsValue};

use super::lefticon::LeftIcon;
use crate::{
    app::LoggedInSignal, home::create_server::CreateServerPopup, utils::{invoke, ActiveServer, ActiveServerSignal}
};

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
    let active_server_signal =
        context::use_context::<ActiveServerSignal>().expect("ActiveServerSignal context not found");
    let (create_server_popup, set_create_server_popup) = signal(true);

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
                        view! {
                            <LeftIcon
                                img_url=format!("{}/{}", URL, server.image_url)
                                name=server.name
                                onclick=move || {
                                    active_server_signal.set(Some(ActiveServer { id: server.id }));
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
                <CreateServerPopup on_create=move |name, image_url| {
                    // Handle server creation logic here
                } />
            </Show>
        </div>
    }
}
