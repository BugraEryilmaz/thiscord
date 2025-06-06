use crate::utils::*;

use leptos::leptos_dom::logging;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{DownloadProgress, FromEvent, UpdateState};
use wasm_bindgen::prelude::*;

async fn listen_update_state(set_update_state: WriteSignal<UpdateState>) {
    let handler = Closure::<dyn FnMut(JsValue)>::new(move |val: JsValue| {
        logging::console_log(format!("Received update state event: {:?}", val).as_str());
        let new_state: UpdateState =
            UpdateState::from_event_js(val).expect("Failed to deserialize UpdateState");
        set_update_state.set(new_state);
    });
    listen("update_state", handler.as_ref().unchecked_ref()).await;
    handler.forget(); // Prevents the closure from being garbage collected
}

async fn listen_download_progress(set_download_progress: WriteSignal<DownloadProgress>) {
    let handler = Closure::<dyn FnMut(JsValue)>::new(move |val: JsValue| {
        logging::console_log(format!("Received update state event: {:?}", val).as_str());
        let new_state =
            DownloadProgress::from_event_js(val).expect("Failed to deserialize UpdateState");
        set_download_progress.set(new_state);
    });
    listen("download_progress", handler.as_ref().unchecked_ref()).await;
    handler.forget(); // Prevents the closure from being garbage collected
}

#[component]
pub fn App() -> impl IntoView {
    let (update_state, set_update_state) = signal(UpdateState::Checking);
    let (download_progress, set_download_progress) = signal(DownloadProgress(0));

    spawn_local(listen_update_state(set_update_state));
    spawn_local(listen_download_progress(set_download_progress));

    view! {
        <main class="container">
            <Show
                when=move || matches!(update_state.get(), UpdateState::Downloading)
                fallback=|| {}
            >
                <p>{format!("Downloading Progress: {}%", download_progress.get().0)}</p>
            </Show>
            <Show
                when=move || matches!(update_state.get(), UpdateState::Completed)
                fallback=move || {
                    view! {
                        <div>
                            <p>{move || update_state.get().to_string()}</p>
                            <button onclick=move || spawn_local(async move {
                                invoke("check_updates", JsValue::UNDEFINED).await;
                            })>"Check for Updates"</button>
                        </div>
                    }
                }
            >
                <p>"Updates completed successfully."</p>
            </Show>
        </main>
    }
}
