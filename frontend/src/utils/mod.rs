
use js_sys::Function;
use leptos::task::spawn_local;
use serde::de::DeserializeOwned;
use shared::FromEvent;
use wasm_bindgen::prelude::*;


#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"])]
    #[wasm_bindgen(catch)]
    pub async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"])]
    pub async fn listen(event: &str, handler: &Function) -> JsValue;
}

pub fn create_listener<F, T>(event: &'static str, mut handler: F)
where
    F: FnMut(T) + 'static,
    T: FromEvent + DeserializeOwned,
{
    spawn_local(async move {
        let closure= Closure::<dyn FnMut(JsValue)>::new(move |val: JsValue| {
            let new_value = T::from_event_js(val).expect("Failed to deserialize event");
            handler(new_value);
        });
        listen(event, closure.as_ref().unchecked_ref()).await;
        closure.forget(); // Prevents the closure from being garbage collected
    });
}
