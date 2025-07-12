use leptos::{context, logging::log, prelude::*, task::spawn_local};
use front_shared::{URL};
use shared::models::Server;
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    app::LoggedInSignal,
    home::create_server::{CreateServerPopup, JoinServerPopup},
    utils::{hover_menu::{HoverMenu, HoverMenuDirection, HoverMenuTrigger}, invoke, popup::{Popup, PopupBackgroundStyle}},
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
    let create_server_popup = RwSignal::new(false);
    let join_server_popup = RwSignal::new(false);

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
                        create_server_popup.set(true);
                    }
                />
                <LeftIcon
                    img_url="/public/join_server.svg".to_string()
                    name="Join Server".to_string()
                    onclick=move || {
                        join_server_popup.set(true);
                    }
                />
            </ul>
            <Popup
                visible=create_server_popup
                background_style=vec![PopupBackgroundStyle::Blur, PopupBackgroundStyle::Brightness]
            >
                <CreateServerPopup on_create=move || {
                    create_server_popup.set(false);
                    is_logged_in_signal.update(|_| {});
                } />
            </Popup>
            <Popup
                visible=join_server_popup
                background_style=vec![PopupBackgroundStyle::Blur, PopupBackgroundStyle::Brightness]
            >
                <JoinServerPopup on_join=move || {
                    join_server_popup.set(false);
                    is_logged_in_signal.update(|_| {});
                } />
            </Popup>
        </div>
    }
}

#[component]
pub fn LeftIcon(img_url: String, name: String, mut onclick: impl FnMut() -> () + 'static) -> impl IntoView {
    view! {
        <div class=style::server_list_item>
            <HoverMenu
                on:click=move |_| {
                    onclick();
                }
                item=view! {
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
                }
                popup = view! {
                    <span class=style::server_list_name>
                        {name}
                    </span>
                }
                direction = HoverMenuDirection::Right
                trigger = HoverMenuTrigger::Hover
            />
        </div>
}
    
}
