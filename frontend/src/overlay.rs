use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::overlay::OverlaySnapshot;

#[component]
pub fn VoiceOverlay() -> impl IntoView {
    let snapshot = RwSignal::new(OverlaySnapshot::default());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let toggle = move |action| {
        busy.set(true);
        leptos::task::spawn_local(async move {
            match native::<()>("overlay_toggle_audio", json!({"action":action})).await {
                Ok(()) => {
                    error.set(String::new());
                    if let Ok(value) = native("overlay_snapshot", json!({})).await {
                        snapshot.set(value);
                    }
                }
                Err(message) => error.set(message),
            }
            busy.set(false);
        });
    };
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    if desktop() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    loop {
                        snapshot.set(
                            native("overlay_snapshot", json!({}))
                                .await
                                .unwrap_or_default(),
                        );
                        gloo_timers::future::TimeoutFuture::new(100).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
    view! {
        <main class="voice-overlay" aria-label=move || crate::i18n::t("Active voice chat")>
            <For each=move || snapshot.get().participants
                key=|p| p.account_id
                children=move |p| {
                let id = p.account_id;
                let person = Signal::derive(move || snapshot.with(|s| s.participants.iter().find(|p| p.account_id == id).cloned()).unwrap_or_else(|| p.clone()));
                let accessible_name = move || {
                    let p = person.get();
                    let state = if p.deafened { crate::i18n::t("Deafened") } else if p.muted { crate::i18n::t("Muted") }
                        else if p.speaking { crate::i18n::t("Speaking") } else { crate::i18n::t("Listening") };
                    thiscord_frontend::localized_format!(crate::i18n::language(), "{}: {state}", p.name, state=state)
                };
                view! {
                    <div class="overlay-person" class:overlay-self=move || person.get().is_self class:overlay-speaking=move || person.get().speaking aria-label=accessible_name>
                        <crate::avatar::Avatar class="overlay-avatar" name=Signal::derive(move || person.get().name) avatar_id=Signal::derive(move || person.get().avatar_id)/>
                        <span class="overlay-label"><span class="truncate">{move || person.get().name}</span>
                            <span class="overlay-status" aria-hidden="true">{move || { let p=person.get(); if p.deafened { "⊘" } else if p.muted { "×" } else if p.speaking { "•" } else { "" } }}</span>
                        </span>
                        <Show when=move || person.get().is_self>
                            <span class="overlay-actions">
                                <button aria-pressed=move || person.get().muted disabled=move || busy.get()
                                    on:click=move |_| toggle(thiscord_shared::audio::AudioToggle::Mute)>
                                    {move || if person.get().muted { crate::i18n::t("Unmute") } else { crate::i18n::t("Mute") }}
                                </button>
                                <button aria-pressed=move || person.get().deafened disabled=move || busy.get()
                                    on:click=move |_| toggle(thiscord_shared::audio::AudioToggle::Deafen)>
                                    {move || if person.get().deafened { crate::i18n::t("Undeafen") } else { crate::i18n::t("Deafen") }}
                                </button>
                            </span>
                        </Show>
                    </div>
                }
            }/>
            <Show when=move || !error.get().is_empty()><p class="rounded bg-slate-900 p-2 text-sm text-red-300" role="status">{move || crate::i18n::message(error.get())}</p></Show>
        </main>
    }
}

#[component]
pub fn OverlaySettings(
    #[prop(default = crate::audio_control::AudioControl::default())]
    control: crate::audio_control::AudioControl,
) -> impl IntoView {
    let enabled = RwSignal::new(true);
    let busy = RwSignal::new(true);
    let error = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    control.poll::<bool>("overlay_enabled", move |result| {
        if loaded.get_untracked() && busy.get_untracked() {
            return;
        }
        match result {
            Ok(value) => {
                enabled.set(value);
                loaded.set(true);
                busy.set(false);
            }
            Err(message) => error.set(message),
        }
    });
    view! {
        <section class="space-y-2 rounded-lg border border-white/15 p-4">
            <h3 class="font-semibold">{move || crate::i18n::t("In-game voice overlay")}</h3>
            <label class="flex items-center gap-3">
                <input type="checkbox" prop:checked=move || enabled.get() disabled=move || busy.get()
                    on:change=move |ev| {
                        let value = event_target_checked(&ev);
                        busy.set(true);
                        leptos::task::spawn_local(async move {
                            let result = control.invoke::<()>("overlay_enable", json!({"enabled": value})).await;
                            if busy.is_disposed() { return; }
                            match result {
                                Ok(()) => { enabled.set(value); error.set(String::new()); },
                                Err(message) => error.set(message),
                            }
                            busy.set(false);
                        });
                    }/>
                {move || crate::i18n::t("Show voice participants when Thiscord is out of focus")}
            </label>
            <p class="text-sm text-white/60">{move || crate::i18n::t("Hover over your own entry at the top left to mute or deafen. The rest of the roster passes clicks through. Green rings show who is talking. Use windowed or borderless games; exclusive fullscreen is not supported. Applies until you restart Thiscord.")}</p>
            <p class="text-sm text-red-300" role="status">{move || crate::i18n::message(error.get())}</p>
        </section>
    }
}
