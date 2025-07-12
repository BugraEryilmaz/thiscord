pub mod hover_menu;
pub mod popup;
pub mod dropdown;

use js_sys::Function;
use leptos::{logging::log, task::spawn_local};
use serde::de::DeserializeOwned;
use serde_wasm_bindgen::from_value;
use front_shared::FromEvent;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    #[wasm_bindgen(catch)]
    pub async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"])]
    pub async fn listen(event: &str, handler: &Function) -> JsValue;
    #[wasm_bindgen(js_namespace = ["window", "__TAURI_INTERNALS__"])]
    pub fn convertFileSrc(path: &str) -> JsValue;
}

/**
 *  Converts a file path to a URL that can be used in the frontend.
 *  This is a wrapper around the Tauri's `convertFileSrc` function.
 *  
 */
pub fn convert_file_src(path: &str) -> String {
    let converted_url = convertFileSrc(path);
    from_value::<String>(converted_url).unwrap_or_else(|_| {
        log!("Failed to convert file src");
        "/public/leptos.svg".to_string()
    })
}

pub fn create_listener<F, T>(event: &'static str, mut handler: F)
where
    F: FnMut(T) + 'static,
    T: FromEvent + DeserializeOwned,
{
    spawn_local(async move {
        let closure = Closure::<dyn FnMut(JsValue)>::new(move |val: JsValue| {
            let new_value = T::from_event_js(val).expect("Failed to deserialize event");
            handler(new_value);
        });
        listen(event, closure.as_ref().unchecked_ref()).await;
        closure.forget(); // Prevents the closure from being garbage collected
    });
}
