use leptos::{logging::log, prelude::*, task::spawn_local};
use serde_wasm_bindgen::from_value;
use wasm_bindgen::JsValue;

use crate::utils::{convert_file_src, invoke};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

#[component]
pub fn CreateServerPopup(on_create: impl FnMut(String, String) -> () + 'static) -> impl IntoView {
    let name_ref = NodeRef::new();
    let (img_url, set_img_url) = signal(Option::<String>::None);

    view! {
        <div class=style::create_server_popup>
            <h2>"Create Server"</h2>
            <form on:submit=move |event| {
                event.prevent_default();
            }>
                <button
                    type="button"
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
                >
                    "Select Image"
                </button>
                <Show when=move || img_url.get().is_some()>
                    <img src=move || convert_file_src(img_url.get().unwrap().as_str()) />
                </Show>

                <input type="text" placeholder="Server Name" required node_ref=name_ref />

                <button type="submit">"Create"</button>
            </form>
        </div>
    }
}
