use crate::audio_control::AudioControl;
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::audio::{AudioShortcutStatus, AudioShortcuts};

#[component]
pub(super) fn ShortcutSettings(
    #[prop(default = AudioControl::default())] control: AudioControl,
    #[prop(optional)] on_dirty: Option<Callback<bool>>,
) -> impl IntoView {
    let mute = RwSignal::new(String::new());
    let deafen = RwSignal::new(String::new());
    let busy = RwSignal::new(true);
    let message = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let dirty = RwSignal::new(false);
    Effect::new(move |_| {
        let changed = dirty.get();
        if let Some(callback) = on_dirty {
            callback.run(changed);
        }
    });
    on_cleanup(move || {
        if let Some(callback) = on_dirty {
            callback.run(false);
        }
    });
    control.poll::<AudioShortcutStatus>("audio_shortcuts", move |result| {
        if loaded.get_untracked() && (busy.get_untracked() || dirty.get_untracked()) {
            return;
        }
        match result {
            Ok(status) => {
                mute.set(status.shortcuts.mute);
                deafen.set(status.shortcuts.deafen);
                message.set(status.error.unwrap_or_default());
                loaded.set(true);
                busy.set(false);
            }
            Err(error) => message.set(error),
        }
    });
    view! {
        <section class="space-y-3 rounded-lg border border-white/15 p-4" aria-label=move || crate::i18n::t("Voice keyboard shortcuts")>
            <h3 class="font-semibold">{move || crate::i18n::t("Keyboard shortcuts")}</h3>
            <p class="text-sm text-white/60">{move || crate::i18n::t("Global desktop shortcuts work while other apps are focused. Enter a combination such as Control+Shift+M, Alt+M or F8. Use Super for the Command / Windows key. Clear a field to disable that shortcut. Ctrl+Shift+Space is reserved for push-to-talk. Requires Windows, macOS or Linux/X11.")}</p>
            <label class="block space-y-2"><span>{move || crate::i18n::t("Toggle mute")}</span>
                <input class="w-full rounded bg-slate-900 p-2" type="text" maxlength="128" spellcheck="false" placeholder=move || crate::i18n::t("Disabled") disabled=move || busy.get()
                    prop:value=move || mute.get() on:input=move |ev| {dirty.set(true); mute.set(event_target_value(&ev));}/>
            </label>
            <label class="block space-y-2"><span>{move || crate::i18n::t("Toggle deafen")}</span>
                <input class="w-full rounded bg-slate-900 p-2" type="text" maxlength="128" spellcheck="false" placeholder=move || crate::i18n::t("Disabled") disabled=move || busy.get()
                    prop:value=move || deafen.get() on:input=move |ev| {dirty.set(true); deafen.set(event_target_value(&ev));}/>
            </label>
            <div class="flex gap-3">
                <button class="rounded bg-brand px-3 py-2 text-sm disabled:opacity-50" disabled=move || busy.get() on:click=move |_| {
                    busy.set(true);
                    let settings = AudioShortcuts { mute: mute.get_untracked().trim().to_owned(), deafen: deafen.get_untracked().trim().to_owned() };
                    leptos::task::spawn_local(async move {
                        let result = control.invoke::<()>("audio_shortcuts_save", json!({"settings":settings})).await;
                        if busy.is_disposed() { return; }
                        match result {
                            Ok(()) => { dirty.set(false); message.set("Shortcuts saved".into()); },
                            Err(error) => message.set(error),
                        }
                        busy.set(false);
                    });
                }>{move || crate::i18n::t("Save shortcuts")}</button>
                <button class="rounded bg-white/10 px-3 py-2 text-sm" disabled=move || busy.get() on:click=move |_| {
                    dirty.set(true); let defaults = AudioShortcuts::default(); mute.set(defaults.mute); deafen.set(defaults.deafen);
                }>{move || crate::i18n::t("Reset to defaults")}</button>
            </div>
            <p class="text-sm text-white/70" role="status">{move || crate::i18n::message(message.get())}</p>
        </section>
    }
}
