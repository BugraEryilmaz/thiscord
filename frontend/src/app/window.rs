use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;

#[component]
pub fn WindowSettings() -> impl IntoView {
    let enabled = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        let result = native::<bool>("window_close_to_tray", json!({})).await;
        if loaded.try_get_untracked().is_none() {
            return;
        }
        match result {
            Ok(value) => {
                enabled.set(value);
                loaded.set(true);
            }
            Err(message) => error.set(message),
        }
    });
    view! {
        <div class="settings-card">
            <h2 class="settings-panel-title text-xl font-semibold">{move || crate::i18n::t("Desktop")}</h2>
            <label class="flex items-center gap-3">
                <input type="checkbox" prop:checked=move || enabled.get()
                    disabled=move || !loaded.get() || busy.get()
                    on:change=move |ev| {
                        if busy.get_untracked() { return; }
                        let previous = enabled.get_untracked();
                        let value = event_target_checked(&ev);
                        enabled.set(value);
                        busy.set(true);
                        error.set(String::new());
                        leptos::task::spawn_local(async move {
                            let result = native::<()>("window_set_close_to_tray", json!({"enabled":value})).await;
                            if busy.try_get_untracked().is_none() { return; }
                            if let Err(message) = result {
                                enabled.set(previous);
                                error.set(message);
                            }
                            busy.set(false);
                        });
                    }/>
                {move || crate::i18n::t("Keep Thiscord running when I close the window")}
            </label>
            <p class="settings-hint">{move || crate::i18n::t("Calls and screen sharing continue in the background. Reopen Thiscord from the tray, menu bar, or taskbar. Choose Quit Thiscord to exit.")}</p><p class="settings-hint">{move || crate::i18n::t("Off by default. Changes save automatically on this computer.")}</p>
            <Show when=move || busy.get()><p role="status">{move || crate::i18n::t("Saving…")}</p></Show>
            <p role="status" class="text-red-300">{move || crate::i18n::message(error.get())}</p>
        </div>
    }
}
