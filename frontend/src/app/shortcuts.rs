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
    let overlay_visibility = RwSignal::new(String::new());
    let overlay_corner = RwSignal::new(String::new());
    let overlay_unlock = RwSignal::new(String::new());
    let busy = RwSignal::new(true);
    let message = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let dirty = RwSignal::new(false);
    // The web client stores these in this browser and has no overlay.
    let browser = crate::account_client::web_app() && !control.is_remote();
    let load = move |shortcuts: AudioShortcuts| {
        mute.set(shortcuts.mute);
        deafen.set(shortcuts.deafen);
        overlay_visibility.set(shortcuts.overlay_visibility);
        overlay_corner.set(shortcuts.overlay_corner);
        overlay_unlock.set(shortcuts.overlay_unlock);
    };
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
                load(status.shortcuts);
                message.set(status.error.unwrap_or_default());
                loaded.set(true);
                busy.set(false);
            }
            Err(error) => message.set(error),
        }
    });
    let field = move |label: fn() -> &'static str, value: RwSignal<String>| {
        view! {
            <label class="block space-y-2"><span>{label}</span>
                <input class="w-full rounded bg-slate-900 p-2" type="text" maxlength="128" spellcheck="false" placeholder=move || crate::i18n::t("Disabled") disabled=move || busy.get()
                    prop:value=move || value.get() on:input=move |ev| {dirty.set(true); value.set(event_target_value(&ev));}/>
            </label>
        }
    };
    view! {
        <section class="space-y-3 rounded-lg border border-white/15 p-4" aria-label=move || crate::i18n::t("Voice keyboard shortcuts")>
            <h3 class="font-semibold">{move || crate::i18n::t("Keyboard shortcuts")}</h3>
            <p class="text-sm text-white/60">{move || if browser {
                crate::i18n::t("Browsers can only see keys while this Thiscord tab is focused; shortcuts do not work from other apps or tabs (use the desktop app for global shortcuts). Enter a combination such as Control+Shift+M, Alt+M or F8. Use Super for the Command / Windows key. Clear a field to disable that shortcut. Ctrl+Shift+Space is push-to-talk. Keys without Control, Alt or Super are ignored while you type.")
            } else {
                crate::i18n::t("Global desktop shortcuts work while other apps are focused. Enter a combination such as Control+Shift+M, Alt+M or F8. Use Super for the Command / Windows key. Clear a field to disable that shortcut. Ctrl+Shift+Space is reserved for push-to-talk. Requires Windows, macOS or Linux/X11.")
            }}</p>
            <fieldset class="space-y-3">
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Voice")}</legend>
                <div class="grid gap-3 sm:grid-cols-2">
                    {field(|| crate::i18n::t("Toggle mute"), mute)}
                    {field(|| crate::i18n::t("Toggle deafen"), deafen)}
                </div>
            </fieldset>
            <Show when=move || !browser>
            <fieldset class="space-y-3">
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Overlay")}</legend>
                <p class="text-sm text-white/60">{move || crate::i18n::t("Off until you choose a combination, so other apps keep their keys.")}</p>
                <div class="grid gap-3 sm:grid-cols-3">
                    {field(|| crate::i18n::t("Show or hide overlay"), overlay_visibility)}
                    {field(|| crate::i18n::t("Move to next corner"), overlay_corner)}
                    {field(|| crate::i18n::t("Unlock to drag"), overlay_unlock)}
                </div>
            </fieldset>
            </Show>
            <div class="flex gap-3">
                <button class="rounded bg-brand px-3 py-2 text-sm disabled:opacity-50" disabled=move || busy.get() on:click=move |_| {
                    busy.set(true);
                    let text = |value: RwSignal<String>| value.get_untracked().trim().to_owned();
                    let settings = AudioShortcuts {
                        mute: text(mute),
                        deafen: text(deafen),
                        overlay_visibility: text(overlay_visibility),
                        overlay_corner: text(overlay_corner),
                        overlay_unlock: text(overlay_unlock),
                    };
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
                    dirty.set(true); load(AudioShortcuts::default());
                }>{move || crate::i18n::t("Reset to defaults")}</button>
            </div>
            <p class="text-sm text-white/70" role="status">{move || crate::i18n::message(message.get())}</p>
        </section>
    }
}
