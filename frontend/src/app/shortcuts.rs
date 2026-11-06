use crate::audio_control::AudioControl;
use leptos::prelude::*;
use serde_json::json;
use thiscord_frontend::keyboard::{AppShortcuts, COMMAND_KEY, Command};
use thiscord_frontend::web_shortcuts::{Chord, Pressed, Recorded, TYPING_KEY, labels, record};
use thiscord_shared::audio::{AudioShortcutStatus, AudioShortcuts};

/// Key caps for a binding, or "Not set".
fn keys(text: String) -> impl IntoView {
    let parts = labels(&text);
    if parts.is_empty() {
        return view! {<span class="text-white/60">{move || crate::i18n::t("Not set")}</span>}
            .into_any();
    }
    view! {
        <span class="flex flex-wrap items-center gap-1">
            {parts.into_iter().map(|part| view! {<kbd>{part}</kbd>}).collect_view()}
        </span>
    }
    .into_any()
}

/// Which recorded keys a shortcut accepts.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Rule {
    #[default]
    Any,
    /// Push-to-talk: no plain letter, number, space or punctuation key.
    NoTypingKey,
    /// App commands: Ctrl, Alt or Super, or a function key.
    Command,
}

/// Click, then press the combination. Escape or leaving the control cancels;
/// Tab moves on. `label` is a UI catalog key.
#[component]
fn KeyRecorder(
    label: &'static str,
    value: RwSignal<String>,
    busy: RwSignal<bool>,
    dirty: RwSignal<bool>,
    message: RwSignal<String>,
    #[prop(optional)] rule: Rule,
) -> impl IntoView {
    let recording = RwSignal::new(false);
    // Modifiers held so far while recording.
    let held = RwSignal::new(String::new());
    let stop = move || {
        recording.set(false);
        held.set(String::new());
    };
    let keydown = move |e: web_sys::KeyboardEvent| {
        if !recording.get_untracked() {
            return;
        }
        let code = e.code();
        let plain = !(e.ctrl_key() || e.alt_key() || e.shift_key() || e.meta_key());
        if code == "Tab" && plain {
            stop();
            return;
        }
        // Keep the press away from app shortcuts and push-to-talk.
        e.prevent_default();
        e.stop_propagation();
        if e.repeat() {
            return;
        }
        let pressed = Pressed {
            code: &code,
            control: e.ctrl_key(),
            shift: e.shift_key(),
            alt: e.alt_key(),
            meta: e.meta_key(),
        };
        match record(&pressed) {
            Recorded::Pending(modifiers) => held.set(modifiers),
            Recorded::Cancel => stop(),
            Recorded::Unsupported => {
                message.set("That key cannot be used for shortcuts. Try another key.".into())
            }
            Recorded::Chord(text) => {
                let refused = Chord::parse(&text).and_then(|chord| match rule {
                    Rule::NoTypingKey if chord.blocks_typing() => Some(TYPING_KEY),
                    Rule::Command if !chord.while_typing() => Some(COMMAND_KEY),
                    _ => None,
                });
                if let Some(reason) = refused {
                    message.set(reason.into());
                    held.set(String::new());
                    return;
                }
                message.set(String::new());
                value.set(text);
                dirty.set(true);
                stop();
            }
        }
    };
    let keyup = move |e: web_sys::KeyboardEvent| {
        if recording.get_untracked() {
            e.prevent_default();
            e.stop_propagation();
            // Releasing modifiers updates the preview.
            if let Recorded::Pending(modifiers) = record(&Pressed {
                code: "ShiftLeft",
                control: e.ctrl_key(),
                shift: e.shift_key(),
                alt: e.alt_key(),
                meta: e.meta_key(),
            }) {
                held.set(modifiers);
            }
        }
    };
    let described = move || {
        let current = labels(&value.get()).join(" + ");
        let current = if current.is_empty() {
            crate::i18n::t("Not set").to_owned()
        } else {
            current
        };
        format!("{}: {current}", crate::i18n::t(label))
    };
    view! {
        <div class="space-y-2">
            <span class="block">{move || crate::i18n::t(label)}</span>
            <div class="flex items-stretch gap-2">
                <button type="button" class="shortcut-recorder" class:recording=move || recording.get()
                    disabled=move || busy.get()
                    aria-label=described
                    aria-pressed=move || recording.get().to_string()
                    on:click=move |_| {
                        message.set(String::new());
                        held.set(String::new());
                        recording.update(|r| *r = !*r);
                    }
                    on:keydown=keydown on:keyup=keyup on:blur=move |_| stop()>
                    {move || if recording.get() {
                        let preview = held.get();
                        view! {
                            <span class="flex flex-wrap items-center gap-1">
                                {(!preview.is_empty()).then(|| keys(format!("{preview}+…")))}
                                <span class="text-white/70">{move || crate::i18n::t("Press keys… (Esc to cancel)")}</span>
                            </span>
                        }.into_any()
                    } else {
                        keys(value.get()).into_any()
                    }}
                </button>
                <button type="button" class="icon-button shrink-0" disabled=move || busy.get() || value.with(String::is_empty)
                    aria-label=move || format!("{}: {}", crate::i18n::t("Clear shortcut"), crate::i18n::t(label))
                    data-tip=move || crate::i18n::t("Clear shortcut")
                    on:click=move |_| {
                        stop();
                        value.set(String::new());
                        dirty.set(true);
                    }>"×"</button>
            </div>
        </div>
    }
}

#[component]
pub(super) fn ShortcutSettings(
    #[prop(default = AudioControl::default())] control: AudioControl,
    #[prop(optional)] on_dirty: Option<Callback<bool>>,
) -> impl IntoView {
    let push_to_talk = RwSignal::new(String::new());
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
    // App shortcuts belong to the device showing this panel, never a remote one.
    let local = !control.is_remote();
    let saved = super::keyboard::bindings_untracked();
    let app = Command::ALL.map(|command| RwSignal::new(saved.get(command).to_owned()));
    let load_app = move |shortcuts: AppShortcuts| {
        for (command, value) in Command::ALL.into_iter().zip(app) {
            value.set(shortcuts.get(command).to_owned());
        }
    };
    let load = move |shortcuts: AudioShortcuts| {
        push_to_talk.set(shortcuts.push_to_talk);
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
                // Polls repeat every few seconds; keep recorder and save messages.
                if let Some(error) = status.error {
                    message.set(error);
                }
                loaded.set(true);
                busy.set(false);
            }
            Err(error) => message.set(error),
        }
    });
    view! {
        <section class="space-y-3 rounded-lg border border-white/15 p-4" aria-label=move || crate::i18n::t("Voice keyboard shortcuts")>
            <h3 class="font-semibold">{move || crate::i18n::t("Keyboard shortcuts")}</h3>
            <p class="text-sm text-white/60">{move || if browser {
                crate::i18n::t("Click a shortcut, then press the keys you want. Browsers can only see keys while this Thiscord tab is focused; use the desktop app for shortcuts that work from other apps. Keys without Control, Alt or Super are ignored while you type.")
            } else {
                crate::i18n::t("Click a shortcut, then press the keys you want. These work while other apps are focused, and other apps no longer receive those keys. Requires Windows, macOS or Linux/X11.")
            }}</p>
            <Show when=move || local>
            <fieldset class="space-y-3">
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Navigation")}</legend>
                <p class="text-sm text-white/60">{move || crate::i18n::t("These work while Thiscord is focused, including in the message box, and save on this device.")}</p>
                <div class="grid gap-3 sm:grid-cols-3">
                    {Command::ALL.into_iter().zip(app).map(|(command, value)| view! {
                        <KeyRecorder label=command.label() value busy dirty message rule=Rule::Command/>
                    }).collect_view()}
                </div>
            </fieldset>
            </Show>
            <fieldset class="space-y-3">
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Voice")}</legend>
                <div class="grid gap-3 sm:grid-cols-3">
                    <KeyRecorder label="Push to talk" value=push_to_talk busy dirty message rule=Rule::NoTypingKey/>
                    <KeyRecorder label="Toggle mute" value=mute busy dirty message/>
                    <KeyRecorder label="Toggle deafen" value=deafen busy dirty message/>
                </div>
                <p class="text-sm text-white/60">{move || if browser {
                    crate::i18n::t("Hold the push-to-talk key to speak when Transmit mode is Push to talk.")
                } else {
                    crate::i18n::t("Hold the push-to-talk key to speak when Transmit mode is Push to talk and Global push-to-talk is on in Voice & audio. Without a key, use the hold button.")
                }}</p>
            </fieldset>
            <Show when=move || !browser>
            <fieldset class="space-y-3">
                <legend class="mb-2 text-sm font-semibold text-white/80">{move || crate::i18n::t("Overlay")}</legend>
                <p class="text-sm text-white/60">{move || crate::i18n::t("Off until you choose a combination, so other apps keep their keys.")}</p>
                <div class="grid gap-3 sm:grid-cols-3">
                    <KeyRecorder label="Show or hide overlay" value=overlay_visibility busy dirty message/>
                    <KeyRecorder label="Move to next corner" value=overlay_corner busy dirty message/>
                    <KeyRecorder label="Unlock to drag" value=overlay_unlock busy dirty message/>
                </div>
            </fieldset>
            </Show>
            <div class="flex gap-3">
                <button class="rounded bg-brand px-3 py-2 text-sm disabled:opacity-50" disabled=move || busy.get() || !dirty.get() on:click=move |_| {
                    let text = |value: RwSignal<String>| value.get_untracked().trim().to_owned();
                    let settings = AudioShortcuts {
                        mute: text(mute),
                        deafen: text(deafen),
                        overlay_visibility: text(overlay_visibility),
                        overlay_corner: text(overlay_corner),
                        overlay_unlock: text(overlay_unlock),
                        push_to_talk: text(push_to_talk),
                    };
                    if local {
                        let mut shortcuts = AppShortcuts::default();
                        for (command, value) in Command::ALL.into_iter().zip(app) {
                            *shortcuts.get_mut(command) = text(value);
                        }
                        let voice = [&settings.push_to_talk, &settings.mute, &settings.deafen, &settings.overlay_visibility, &settings.overlay_corner, &settings.overlay_unlock].map(String::as_str);
                        if let Err(error) = shortcuts.validate(&voice).map_err(String::from).and_then(|()| super::keyboard::save_bindings(shortcuts)) {
                            message.set(error);
                            return;
                        }
                    }
                    busy.set(true);
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
                    dirty.set(true); load(AudioShortcuts::default()); load_app(AppShortcuts::default());
                }>{move || crate::i18n::t("Reset to defaults")}</button>
            </div>
            <p class="text-sm text-white/70" role="status">{move || crate::i18n::message(message.get())}</p>
        </section>
    }
}
