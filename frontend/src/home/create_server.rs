use leptos::{logging::log, prelude::*, task::spawn_local};
use serde_wasm_bindgen::{from_value, to_value};
use shared::models::{ConnectionString, ServerWithoutID};
use wasm_bindgen::JsValue;

use crate::utils::{convert_file_src, invoke};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

#[component]
pub fn CreateServerPopup(on_create: impl FnMut() -> () + 'static + Clone) -> impl IntoView {
    let name_ref = NodeRef::new();
    let (img_url, set_img_url) = signal(Option::<String>::None);

    view! {
        <div class=style::create_server_popup>
            <form 
            class=style::create_server_form
            on:submit=move |event| {
                event.prevent_default();
                let name = name_ref.get().unwrap().value();
                let img = img_url.get();
                log!("Creating server with name: {}, img: {:?}", name, img);
                let create_server_request = ServerWithoutID {
                    name: name,
                    image_url: img,
                };
                let mut on_create = on_create.clone();
                spawn_local(async move {
                    match invoke("create_server", to_value(&create_server_request).unwrap()).await {
                        Ok(_) => {
                            log!("Server created successfully");
                            on_create();
                        }
                        Err(err) => {
                            log!("Error creating server: {:?}", err);
                        }
                    }
                });
            }

            >
                <h2>"Create Server"</h2>
                <img src=move || {
                    match img_url.get() {
                        Some(url) => convert_file_src(url.as_str()),
                        None => "/public/upload_img.svg".to_string(),
                    }
                } 
                on:click=move |_| {
                        spawn_local(async move {
                            let img_path = invoke("pick_file", JsValue::null()).await.unwrap();
                            let img_path = from_value::<Option<String>>(img_path)
                                .unwrap_or_else(|_| {
                                    log!("Failed to get image path");
                                    None
                                });
                            log!("Selected image path: {:?}", img_path);
                            set_img_url.set(img_path);
                        });
                    }
                />

                <input type="text" placeholder="Server Name" required node_ref=name_ref />

                <button type="submit">"Create"</button>
            </form>
        </div>
    }
}


#[component]
pub fn JoinServerPopup(on_join: impl FnMut() -> () + 'static + Clone) -> impl IntoView {
    let connection_string_ref = NodeRef::new();

    view! {
        <div class=style::create_server_popup>
            <form 
            class=style::create_server_form
            on:submit=move |event| {
                event.prevent_default();
                let connection_string = connection_string_ref.get().unwrap().value();
                log!("Joining server with name: {}", connection_string);
                let join_server_request = ConnectionString {
                    connection_string,
                };
                let mut on_join = on_join.clone();
                spawn_local(async move {
                    match invoke("join_server", to_value(&join_server_request).unwrap()).await {
                        Ok(_) => {
                            log!("Server joined successfully");
                            on_join();
                        }
                        Err(err) => {
                            log!("Error joining server: {:?}", err);
                        }
                    }
                });
            }

            >
                <h2>"Join Server"</h2>

                <input type="text" placeholder="Join String (like OiclsjAz)" required node_ref=connection_string_ref />

                <button type="submit">"Join"</button>
            </form>
        </div>
    }
}
