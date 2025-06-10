use crate::home::Home;
use crate::utils::*;

use leptos::{context, leptos_dom::logging};
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{DownloadProgress, UpdateState};
use wasm_bindgen::prelude::*;

pub type SessionCookieSignal = RwSignal<bool>;

#[component]
pub fn App() -> impl IntoView {
    let (update_state, set_update_state) = signal(UpdateState::Checking);
    let (download_progress, set_download_progress) = signal(DownloadProgress(0));
    let session_cookie = RwSignal::new(false);

    create_listener("update_state", move |input: UpdateState| {
        logging::console_log(format!("Update state changed: {:?}", input).as_str());
        set_update_state.set(input);
    });

    create_listener("download_progress", move |input: DownloadProgress| {
        logging::console_log(format!("Download progress changed: {:?}", input).as_str());
        set_download_progress.set(input);
    });

    context::provide_context(session_cookie);

    view! {
        <main>
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
                                invoke("check_updates", JsValue::UNDEFINED).await.unwrap();
                            })>"Check for Updates"</button>
                        </div>
                    }
                }
            >
                <Home />
            </Show>
        </main>
    }
}
