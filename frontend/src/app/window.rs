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
        <div class="space-y-4">
            <h2 class="text-xl font-semibold">"Desktop"</h2>
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
                "Keep Thiscord running when I close the window"
            </label>
            <p class="text-sm text-white/60">"The close button hides Thiscord in the system tray (Windows) or menu bar (macOS). Calls and screen sharing continue in the background."</p>
            <p class="text-sm text-white/60">"On Linux, the window stays minimized in the taskbar so you can restore it even without a system tray."</p>
            <p class="text-sm text-white/60">"Use the Thiscord icon to open the window again or choose Quit Thiscord to exit. Off by default; your choice is saved on this computer."</p>
            <Show when=move || busy.get()><p role="status">"Saving…"</p></Show>
            <p role="status" class="text-red-300">{move || error.get()}</p>
        </div>
    }
}
