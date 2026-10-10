use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::overlay::OverlaySnapshot;

#[component]
pub fn VoiceOverlay() -> impl IntoView {
    let snapshot = RwSignal::new(OverlaySnapshot::default());
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
        <main class="voice-overlay" aria-label="Active voice chat">
            <For each=move || snapshot.get().participants
                key=|p| p.account_id
                children=move |p| {
                let id = p.account_id;
                let person = Signal::derive(move || snapshot.with(|s| s.participants.iter().find(|p| p.account_id == id).cloned()).unwrap_or_else(|| p.clone()));
                let accessible_name = move || {
                    let p = person.get();
                    let state = if p.deafened { "Deafened" } else if p.muted { "Muted" }
                        else if p.speaking { "Speaking" } else { "Listening" };
                    format!("{}: {state}", p.name)
                };
                view! {
                    <div class="overlay-person" class:overlay-speaking=move || person.get().speaking aria-label=accessible_name>
                        <crate::avatar::Avatar class="overlay-avatar" name=Signal::derive(move || person.get().name) avatar_id=Signal::derive(move || person.get().avatar_id)/>
                        <span class="overlay-label"><span class="truncate">{move || person.get().name}</span>
                            <span class="overlay-status" aria-hidden="true">{move || { let p=person.get(); if p.deafened { "⊘" } else if p.muted { "×" } else if p.speaking { "•" } else { "" } }}</span>
                        </span>
                    </div>
                }
            }/>
        </main>
    }
}

#[component]
pub fn OverlaySettings() -> impl IntoView {
    let enabled = RwSignal::new(true);
    let busy = RwSignal::new(true);
    let error = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        match native("overlay_enabled", json!({})).await {
            Ok(value) => {
                enabled.set(value);
                busy.set(false);
            }
            Err(message) => error.set(message),
        }
    });
    view! {
        <section class="space-y-2 rounded-lg border border-white/15 p-4">
            <h3 class="font-semibold">"In-game voice overlay"</h3>
            <label class="flex items-center gap-3">
                <input type="checkbox" prop:checked=move || enabled.get() disabled=move || busy.get()
                    on:change=move |ev| {
                        let value = event_target_checked(&ev);
                        busy.set(true);
                        leptos::task::spawn_local(async move {
                            match native::<()>("overlay_enable", json!({"enabled": value})).await {
                                Ok(()) => { enabled.set(value); error.set(String::new()); },
                                Err(message) => error.set(message),
                            }
                            busy.set(false);
                        });
                    }/>
                "Show voice participants when Thiscord is out of focus"
            </label>
            <p class="text-sm text-white/60">"Click-through roster at the top left of this display. Green rings show who is talking. Use windowed or borderless games; exclusive fullscreen is not supported. Applies until you restart Thiscord."</p>
            <p class="text-sm text-red-300" role="status">{move || error.get()}</p>
        </section>
    }
}
