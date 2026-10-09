use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;

#[component]
pub fn DiagnosticsSettings() -> impl IntoView {
    let enabled = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        let result = native::<bool>("audio_diagnostics_enabled", json!({})).await;
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
        <section class="space-y-2 rounded-lg border border-white/15 p-4 text-sm text-white/70">
            <h3 class="font-semibold text-white">"Diagnostic logging"</h3>
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
                            let result = native::<()>("audio_diagnostics_enable", json!({"enabled":value})).await;
                            if busy.try_get_untracked().is_none() { return; }
                            if let Err(message) = result {
                                enabled.set(previous);
                                error.set(message);
                            }
                            busy.set(false);
                        });
                    }/>
                "Enable diagnostic logging"
            </label>
            <p>"Off by default. Saves technical events and device names on this computer, up to 6 MiB. No voice recordings, messages or credentials. Your choice is remembered across restarts."</p>
            <p>"Enable before reproducing a voice issue, then share the audio-diagnostics files if needed. Turning logging off keeps existing files. Logs are never uploaded automatically."</p>
            <button class="underline" on:click=move |_| leptos::task::spawn_local(async move {
                let result = native::<()>("audio_diagnostics_folder", json!({})).await;
                if error.try_get_untracked().is_some() {
                    error.set(result.err().unwrap_or_default());
                }
            })>"Open diagnostic logs"</button>
            <Show when=move || busy.get()><p role="status">"Saving…"</p></Show>
            <p role="status" class="text-red-300">{move || error.get()}</p>
        </section>
    }
}
