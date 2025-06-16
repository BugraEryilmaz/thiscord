use leptos::{logging::log, prelude::*};
use wasm_bindgen::JsCast;

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

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
