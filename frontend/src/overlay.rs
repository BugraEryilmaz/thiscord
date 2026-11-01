use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::overlay::{
    MAX_OFFSET, OverlayAction, OverlayCorner, OverlaySettings, OverlaySnapshot, OverlayStatus,
};
use wasm_bindgen::JsCast;

fn overlay_action(action: OverlayAction, error: RwSignal<String>) {
    leptos::task::spawn_local(async move {
        let result = native::<()>("overlay_action", json!({ "action": action })).await;
        if error.is_disposed() {
            return;
        }
        error.set(result.err().unwrap_or_default());
    });
}

#[component]
fn EyeIcon(#[prop(into)] open: Signal<bool>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/>
            <circle cx="12" cy="12" r="3"/>
            <Show when=move || !open.get()><path d="M3 3l18 18"/></Show>
        </svg>
    }
}

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
                    if let Ok(Some(value)) =
                        native::<Option<OverlaySnapshot>>("overlay_snapshot", json!({})).await
                    {
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
                        // `Ok(None)`: hidden, keep the last roster. Only changed
                        // snapshots re-run the per-participant views.
                        let next =
                            match native::<Option<OverlaySnapshot>>("overlay_snapshot", json!({}))
                                .await
                            {
                                Ok(None) => None,
                                Ok(Some(value)) => Some(value),
                                Err(_) => Some(OverlaySnapshot::default()),
                            };
                        if let Some(next) = next
                            && snapshot.with_untracked(|current| *current != next)
                        {
                            snapshot.set(next);
                        }
                        gloo_timers::future::TimeoutFuture::new(100).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
    let layout = Memo::new(move |_| snapshot.with(|s| s.layout));
    let roster = move || !layout.get().hidden || layout.get().unlocked;
    // In positioning mode the whole overlay is a drag surface except its buttons.
    let start_drag = move |ev: leptos::ev::PointerEvent| {
        let on_button = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|e| e.closest("button").ok().flatten())
            .is_some();
        if layout.get_untracked().unlocked && ev.button() == 0 && !on_button {
            ev.prevent_default();
            leptos::task::spawn_local(async move {
                if let Err(message) = native::<()>("overlay_start_drag", json!({})).await {
                    error.set(message);
                }
            });
        }
    };
    view! {
        <main class="voice-overlay" aria-label=move || crate::i18n::t("Active voice chat")
            class:overlay-bottom=move || layout.get().corner.bottom()
            class:overlay-right=move || layout.get().corner.right()
            class:overlay-unlocked=move || layout.get().unlocked
            on:pointerdown=start_drag>
            <Show when=move || layout.get().unlocked>
                <div class="overlay-move-bar">
                    <span>{move || crate::i18n::t("Drag to move")}</span>
                    <button on:click=move |_| overlay_action(OverlayAction::NextCorner, error)>{move || crate::i18n::t("Next corner")}</button>
                    <button class="overlay-done" on:click=move |_| overlay_action(OverlayAction::ToggleUnlock, error)>{move || crate::i18n::t("Done")}</button>
                </div>
            </Show>
            <Show when=move || layout.get().visibility_button && !layout.get().unlocked>
                <button class="overlay-visibility" aria-pressed=move || layout.get().hidden
                    aria-label=move || if layout.get().hidden { crate::i18n::t("Show voice overlay") } else { crate::i18n::t("Hide voice overlay") }
                    title=move || if layout.get().hidden { crate::i18n::t("Show voice overlay") } else { crate::i18n::t("Hide voice overlay") }
                    on:click=move |_| overlay_action(OverlayAction::ToggleVisibility, error)>
                    <EyeIcon open=Signal::derive(move || !layout.get().hidden)/>
                </button>
            </Show>
            <Show when=roster>
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
                <Show when=move || layout.get().unlocked && snapshot.with(|s| s.participants.is_empty())>
                    <p class="overlay-placeholder">{move || crate::i18n::t("Voice participants appear here while you are in a call.")}</p>
                </Show>
            </Show>
            <Show when=move || !error.get().is_empty()><p class="overlay-error" role="status">{move || crate::i18n::message(error.get())}</p></Show>
        </main>
    }
}

/// The admin relay controls only the on/off preference of a user's desktop.
#[component]
fn RemoteOverlayToggle(control: crate::audio_control::AudioControl) -> impl IntoView {
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
            <p class="text-sm text-white/60">{move || crate::i18n::t("Overlay position is set on the user's computer.")}</p>
            <p class="text-sm text-red-300" role="status">{move || crate::i18n::message(error.get())}</p>
        </section>
    }
}

#[component]
pub fn OverlaySettings(
    #[prop(default = crate::audio_control::AudioControl::default())]
    control: crate::audio_control::AudioControl,
) -> impl IntoView {
    if control.is_remote() {
        return view! { <RemoteOverlayToggle control=control/> }.into_any();
    }
    view! { <LocalOverlaySettings control=control/> }.into_any()
}

#[component]
fn LocalOverlaySettings(control: crate::audio_control::AudioControl) -> impl IntoView {
    let status = RwSignal::new(None::<OverlayStatus>);
    let busy = RwSignal::new(true);
    // Distances being typed; polling must not overwrite them before they save.
    let dirty = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let offset_x = RwSignal::new(String::new());
    let offset_y = RwSignal::new(String::new());
    let accept = move |value: OverlayStatus| {
        offset_x.set(value.settings.offset_x.to_string());
        offset_y.set(value.settings.offset_y.to_string());
        status.set(Some(value));
    };
    let refresh = move || {
        leptos::task::spawn_local(async move {
            let result = native::<OverlayStatus>("overlay_settings", json!({})).await;
            if status.is_disposed() || dirty.get_untracked() {
                return;
            }
            if let Ok(value) = result {
                accept(value);
            }
        });
    };
    // Follow Done/shortcut changes promptly while positioning mode is on.
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                loop {
                    gloo_timers::future::TimeoutFuture::new(400).await;
                    let unlocked = status
                        .try_get_untracked()
                        .flatten()
                        .is_some_and(|s| s.layout.unlocked);
                    if unlocked && !busy.get_untracked() {
                        refresh();
                    }
                }
            },
            registration,
        )
        .await;
    });
    control.poll::<OverlayStatus>("overlay_settings", move |result| {
        if status.get_untracked().is_some() && (busy.get_untracked() || dirty.get_untracked()) {
            return;
        }
        match result {
            Ok(value) => {
                accept(value);
                busy.set(false);
            }
            Err(message) => error.set(message),
        }
    });
    let settings = move || status.get().map(|s| s.settings).unwrap_or_default();
    let save = move |change: &dyn Fn(&mut OverlaySettings)| {
        let Some(mut next) = status.get_untracked().map(|s| s.settings) else {
            return;
        };
        change(&mut next);
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = native::<()>("overlay_settings_save", json!({ "settings": next })).await;
            if busy.is_disposed() {
                return;
            }
            dirty.set(false);
            match result {
                Ok(()) => error.set(String::new()),
                Err(message) => error.set(message),
            }
            busy.set(false);
            refresh();
        });
    };
    let save_offsets = move || {
        let parse = |value: String| {
            value
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|v| *v <= MAX_OFFSET)
        };
        match (
            parse(offset_x.get_untracked()),
            parse(offset_y.get_untracked()),
        ) {
            (Some(x), Some(y)) => save(&move |s| {
                s.offset_x = x;
                s.offset_y = y;
            }),
            _ => error.set("Enter a distance between 0 and 4096 pixels".into()),
        }
    };
    let act = move |action: OverlayAction| {
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = native::<()>("overlay_action", json!({ "action": action })).await;
            if busy.is_disposed() {
                return;
            }
            error.set(result.err().unwrap_or_default());
            busy.set(false);
            refresh();
        });
    };
    let unlocked = move || status.get().is_some_and(|s| s.layout.unlocked);
    let locked_out = move || busy.get() || status.get().is_none();
    view! {
        <section class="space-y-3 rounded-lg border border-white/15 p-4" aria-label=move || crate::i18n::t("In-game voice overlay")>
            <h3 class="font-semibold">{move || crate::i18n::t("In-game voice overlay")}</h3>
            <label class="flex items-center gap-3 text-sm">
                <input type="checkbox" prop:checked=move || settings().enabled disabled=locked_out
                    on:change=move |ev| { let value = event_target_checked(&ev); save(&move |s| s.enabled = value); }/>
                {move || crate::i18n::t("Show voice participants when Thiscord is out of focus")}
            </label>
            <label class="flex items-center gap-3 text-sm">
                <input type="checkbox" prop:checked=move || settings().visibility_button disabled=move || locked_out() || !settings().enabled
                    on:change=move |ev| { let value = event_target_checked(&ev); save(&move |s| s.visibility_button = value); }/>
                {move || crate::i18n::t("Show a hide/show button on the overlay")}
            </label>
            <p class="settings-hint">{move || crate::i18n::t("The small button sits in the overlay's corner and stays visible while the roster is hidden. It is faint until you point at it.")}</p>

            <fieldset class="space-y-3" disabled=move || locked_out() || !settings().enabled>
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Position")}</legend>
                <div class="grid max-w-xs grid-cols-2 gap-2" role="group" aria-label=move || crate::i18n::t("Corner")>
                    {[OverlayCorner::TopLeft, OverlayCorner::TopRight, OverlayCorner::BottomLeft, OverlayCorner::BottomRight].map(|corner| view! {
                        <button type="button" class="rounded-md border border-white/10 bg-white/5 px-3 py-2 text-sm hover:bg-white/10 aria-pressed:border-indigo-400 aria-pressed:bg-indigo-500/20"
                            aria-pressed=move || settings().corner == corner
                            on:click=move |_| save(&move |s| s.corner = corner)>
                            {move || crate::i18n::t(corner.label())}
                        </button>
                    })}
                </div>
                <label class="settings-field">
                    <span>{move || crate::i18n::t("Display")}</span>
                    <select on:change=move |ev| {
                        let value = event_target_value(&ev);
                        save(&move |s| s.display = (!value.is_empty()).then(|| value.clone()));
                    }>
                        <option value="" selected=move || settings().display.is_none()>{move || crate::i18n::t("Same display as Thiscord")}</option>
                        {move || {
                            let current = settings().display;
                            let mut displays = status.get().map(|s| s.displays).unwrap_or_default();
                            // Keep a disconnected saved display selectable instead of silently dropping it.
                            if let Some(name) = current.as_ref().filter(|n| !displays.iter().any(|d| &d.name == *n)) {
                                displays.push(thiscord_shared::overlay::OverlayDisplay { name: name.clone(), width: 0, height: 0, primary: false });
                            }
                            displays.into_iter().enumerate().map(|(index, display)| {
                                let label = if display.width == 0 {
                                    thiscord_frontend::localized_format!(crate::i18n::language(), "{} (disconnected)", display.name)
                                } else if display.primary {
                                    thiscord_frontend::localized_format!(crate::i18n::language(), "Display {index} · {width}×{height} · primary", index = index + 1, width = display.width, height = display.height)
                                } else {
                                    thiscord_frontend::localized_format!(crate::i18n::language(), "Display {index} · {width}×{height}", index = index + 1, width = display.width, height = display.height)
                                };
                                let selected = current.as_ref() == Some(&display.name);
                                view! { <option value=display.name.clone() selected=selected>{label}</option> }
                            }).collect_view()
                        }}
                    </select>
                </label>
                <div class="grid grid-cols-2 gap-3">
                    <label class="settings-field">
                        <span>{move || if settings().corner.right() { crate::i18n::t("From right edge (px)") } else { crate::i18n::t("From left edge (px)") }}</span>
                        <input type="number" min="0" max=MAX_OFFSET.to_string() step="1" prop:value=move || offset_x.get()
                            on:input=move |ev| { dirty.set(true); offset_x.set(event_target_value(&ev)); }
                            on:change=move |_| save_offsets()/>
                    </label>
                    <label class="settings-field">
                        <span>{move || if settings().corner.bottom() { crate::i18n::t("From bottom edge (px)") } else { crate::i18n::t("From top edge (px)") }}</span>
                        <input type="number" min="0" max=MAX_OFFSET.to_string() step="1" prop:value=move || offset_y.get()
                            on:input=move |ev| { dirty.set(true); offset_y.set(event_target_value(&ev)); }
                            on:change=move |_| save_offsets()/>
                    </label>
                </div>
                <div class="settings-actions justify-start">
                    <button type="button" class=move || if unlocked() { "settings-primary" } else { "settings-secondary" }
                        on:click=move |_| act(OverlayAction::ToggleUnlock)>
                        {move || if unlocked() { crate::i18n::t("Done moving") } else { crate::i18n::t("Move on screen") }}
                    </button>
                    <button type="button" class="settings-secondary" on:click=move |_| {
                        let defaults = OverlaySettings::default();
                        save(&move |s| {
                            s.corner = defaults.corner;
                            s.offset_x = defaults.offset_x;
                            s.offset_y = defaults.offset_y;
                            s.display = None;
                        });
                    }>{move || crate::i18n::t("Reset position")}</button>
                </div>
                <p class="settings-hint">{move || crate::i18n::t("Move on screen shows the overlay so you can drag it anywhere. It snaps to the nearest corner when you press Done. Some Linux (Wayland) and macOS fullscreen setups do not allow apps to move windows.")}</p>
            </fieldset>
            <p class="settings-hint">{move || crate::i18n::t("Hover over your own entry to mute or deafen. The rest of the roster passes clicks through. Green rings show who is talking. Use windowed or borderless games; exclusive fullscreen is not supported.")}</p>
            <p class="text-sm text-red-300" role="status">{move || crate::i18n::message(error.get())}</p>
        </section>
    }
}
