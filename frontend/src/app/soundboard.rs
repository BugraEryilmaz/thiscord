//! Guild soundboard: a call control that plays sounds for everyone in the call,
//! and the server settings page that manages them. On desktop, audio never
//! enters WASM: the native process downloads and mixes clips after the server
//! relays a play. The web client plays them through its WebAudio call graph.
use super::{
    Ui, client,
    icons::{Glyph, Icon},
    settings::ConfirmButton,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use leptos::{prelude::*, wasm_bindgen::JsCast};
use serde_json::json;
use thiscord_shared::{ChannelId, GuildId, SoundId, soundboard::*};

/// Built-in names are translated; uploaded names are the server's own text.
fn label(sound: &Sound) -> String {
    if sound.builtin {
        crate::i18n::message(sound.name.clone())
    } else {
        sound.name.clone()
    }
}

fn uploaded(state: &SoundboardState) -> usize {
    state.sounds.iter().filter(|s| !s.builtin).count()
}

async fn send(ui: Ui, request: &SoundboardRequest) -> Result<SoundboardState, String> {
    let token = ui.token.get_untracked();
    client::api_request(SOUNDBOARD_PATH, request, token.as_deref()).await
}

fn call(ui: Ui) -> Option<(GuildId, ChannelId)> {
    let voice = ui.voice.get();
    ui.voice_location
        .get()
        .filter(|l| voice.soundboard && Some(l.channel_id) == voice.channel_id)
        .map(|l| (l.guild_id, l.channel_id))
}

#[component]
pub(super) fn SoundboardControls(ui: Ui) -> impl IntoView {
    let open = RwSignal::new(false);
    let sounds = RwSignal::new(None::<Result<SoundboardState, String>>);
    let notice = RwSignal::new(String::new());
    let position = RwSignal::new((8.0, 8.0));
    let group = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let available = move || {
        (client::desktop() || super::audio::browser_voice(ui))
            && ui.voice.get().connected
            && call(ui).is_some()
    };
    let close = move || {
        if let Some(button) = trigger.get_untracked() {
            let _ = button.focus();
        }
        open.set(false);
    };
    Effect::new(move |_| {
        if !available() {
            open.set(false);
        }
    });
    // Permissions and sounds can change at any time; reload on every open.
    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        // Voice status updates often; only opening (re)loads the list.
        let Some((guild_id, channel_id)) = untrack(|| call(ui)) else {
            return;
        };
        // Open beside the voice dock, bottom-aligned, so call status stays visible.
        if let Some(button) = trigger.get_untracked() {
            let dock = button
                .closest(".user-dock")
                .ok()
                .flatten()
                .map(|dock| dock.get_bounding_client_rect())
                .unwrap_or_else(|| button.get_bounding_client_rect());
            let size = |value: Result<leptos::wasm_bindgen::JsValue, _>, fallback| {
                value.ok().and_then(|v| v.as_f64()).unwrap_or(fallback)
            };
            let (width, height) = (
                size(window().inner_width(), 800.0),
                size(window().inner_height(), 600.0),
            );
            // The popover is at most 440 px wide (style.css); keep it on screen.
            position.set((
                (dock.right() + 8.0).min(width - 448.0).max(8.0),
                (height - dock.bottom()).max(8.0),
            ));
        }
        sounds.set(None);
        notice.set(String::new());
        leptos::task::spawn_local(async move {
            let request = SoundboardRequest::List {
                guild_id,
                channel_id: Some(channel_id),
            };
            let result = send(ui, &request).await;
            if !sounds.is_disposed()
                && open.get_untracked()
                && untrack(|| call(ui)) == Some((guild_id, channel_id))
            {
                sounds.set(Some(result));
            }
        });
        let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
            let inside = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|target| {
                    group
                        .get_untracked()
                        .is_some_and(|group| group.contains(Some(&target)))
                });
            if !inside {
                open.set(false);
            }
        });
        let resize = window_event_listener(leptos::ev::resize, move |_| open.set(false));
        on_cleanup(move || {
            outside.remove();
            resize.remove();
        });
    });
    let play = move |sound_id: SoundId| {
        let Some((guild_id, channel_id)) = untrack(|| call(ui)) else {
            return;
        };
        notice.set(String::new());
        leptos::task::spawn_local(async move {
            // Native process on desktop, browser voice in the web client.
            let result = crate::audio_control::AudioControl::default()
                .invoke::<()>(
                    "voice_soundboard_play",
                    json!({"guildId":guild_id,"channelId":channel_id,"soundId":sound_id}),
                )
                .await;
            if let Err(error) = result
                && !notice.is_disposed()
            {
                notice.set(error);
            }
        });
    };
    view! {<Show when=available>
        <div node_ref=group class="contents" on:keydown=move |e| {
            if e.key() == "Escape" && open.get_untracked() { e.prevent_default(); e.stop_propagation(); close(); }
        }>
            <button node_ref=trigger type="button" class="icon-button voice-action" aria-label=move || crate::i18n::t("Soundboard")
                data-tip=move || crate::i18n::t("Soundboard") aria-haspopup="dialog" aria-expanded=move || open.get().to_string()
                on:click=move |_| open.update(|value| *value = !*value)><Icon kind=Glyph::Soundboard/></button>
            <Show when=move || open.get()>
                <div class="soundboard-popover" role="dialog" aria-label=move || crate::i18n::t("Soundboard")
                    style:left=move || format!("{}px", position.get().0) style:bottom=move || format!("{}px", position.get().1)>
                    <h2 class="soundboard-title"><Icon kind=Glyph::Soundboard/><span>{move || crate::i18n::t("Soundboard")}</span></h2>
                    {move || match sounds.get() {
                        None => view! {<p class="soundboard-empty">{move || crate::i18n::t("Loading sounds…")}</p>}.into_any(),
                        Some(Err(error)) => view! {<p class="soundboard-empty" role="alert">{error}</p>}.into_any(),
                        Some(Ok(state)) if state.sounds.is_empty() => view! {<p class="soundboard-empty">{move || if state.can_manage {
                            crate::i18n::t("No sounds yet. Add some in Server settings → Soundboard.")
                        } else {
                            crate::i18n::t("This server has no sounds yet.")
                        }}</p>}.into_any(),
                        Some(Ok(state)) => view! {<div class="soundboard-grid">{state.sounds.into_iter().map(|sound| {
                            let id = sound.id;
                            let name = label(&sound);
                            let title = name.clone();
                            view! {<button type="button" class="soundboard-sound" title=title on:click=move |_| play(id)>
                                {sound.emoji.map(|emoji| view! {<span class="soundboard-emoji" aria-hidden="true">{emoji}</span>})}
                                <span class="soundboard-name">{name}</span>
                            </button>}
                        }).collect_view()}</div>}.into_any(),
                    }}
                    <Show when=move || !notice.get().is_empty()><p class="soundboard-notice" role="status">{move || crate::i18n::message(notice.get())}</p></Show>
                </div>
            </Show>
        </div>
    </Show>}
}

/// Sound management for members with Manage server.
#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {
        <For each=move || { ui.server.get().filter(|s| s.can_manage_bots).map(|s| s.guild.id).into_iter().collect::<Vec<_>>() }
            key=|id| *id children=move |guild| view! {<GuildSounds ui=ui guild=guild/>}/>
    }
}

#[component]
fn GuildSounds(ui: Ui, guild: GuildId) -> impl IntoView {
    let state = RwSignal::new(None::<SoundboardState>);
    let name = RwSignal::new(String::new());
    let emoji = RwSignal::new(String::new());
    let file = RwSignal::new(None::<(String, Vec<u8>)>);
    let input = NodeRef::<leptos::html::Input>::new();
    let run = move |request: SoundboardRequest| {
        if ui.busy.get_untracked() {
            return;
        }
        ui.busy.set(true);
        ui.status.set(String::new());
        let session = ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let result = send(ui, &request).await;
            if state.is_disposed() || ui.token.get_untracked() != session {
                ui.busy.set(false);
                return;
            }
            match result {
                Ok(next) => {
                    match request {
                        SoundboardRequest::Upload { .. } => {
                            name.set(String::new());
                            emoji.set(String::new());
                            file.set(None);
                            ui.status.set(crate::i18n::t("Sound added").into());
                        }
                        SoundboardRequest::Delete { .. } => {
                            ui.status.set(crate::i18n::t("Sound deleted").into());
                        }
                        SoundboardRequest::List { .. } => {}
                    }
                    state.set(Some(next));
                }
                Err(error) => {
                    ui.status.set(error);
                    // A stale revision or concurrent change needs a fresh snapshot.
                    if !matches!(request, SoundboardRequest::List { .. })
                        && let Ok(next) = send(
                            ui,
                            &SoundboardRequest::List {
                                guild_id: guild,
                                channel_id: None,
                            },
                        )
                        .await
                        && !state.is_disposed()
                    {
                        state.set(Some(next));
                    }
                }
            }
            ui.busy.set(false);
        });
    };
    run(SoundboardRequest::List {
        guild_id: guild,
        channel_id: None,
    });
    let choose = move |event| {
        let target = event_target::<web_sys::HtmlInputElement>(&event);
        let Some(selected) = target.files().and_then(|files| files.get(0)) else {
            return;
        };
        if selected.size() == 0.0 || selected.size() > MAX_UPLOAD_BYTES as f64 {
            file.set(None);
            target.set_value("");
            ui.status
                .set(crate::i18n::t("Choose a sound file up to 1 MiB").into());
            return;
        }
        let filename = selected.name();
        leptos::task::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(selected.array_buffer()).await {
                Ok(buffer) if !file.is_disposed() => {
                    if name.get_untracked().trim().is_empty() {
                        let stem = filename
                            .rsplit_once('.')
                            .map_or(filename.as_str(), |(stem, _)| stem);
                        name.set(
                            stem.trim()
                                .chars()
                                .take(MAX_NAME_CHARS)
                                .collect::<String>()
                                .trim()
                                .to_owned(),
                        );
                    }
                    file.set(Some((filename, js_sys::Uint8Array::new(&buffer).to_vec())));
                }
                Ok(_) => {}
                Err(_) => ui
                    .status
                    .set(crate::i18n::t("Cannot read this file. Please choose it again").into()),
            }
        });
    };
    let full = move || {
        state
            .get()
            .is_some_and(|s| uploaded(&s) >= s.limit as usize)
    };
    let ready = move || {
        let emoji = emoji.get();
        file.get().is_some()
            && valid_name(name.get().trim())
            && (emoji.trim().is_empty() || valid_emoji(emoji.trim()))
            && !full()
    };
    let upload = move || {
        let (Some(current), Some((_, bytes))) = (state.get_untracked(), file.get_untracked())
        else {
            return;
        };
        let emoji = emoji.get_untracked();
        let emoji = emoji.trim();
        run(SoundboardRequest::Upload {
            guild_id: guild,
            revision: current.revision,
            name: name.get_untracked().trim().to_owned(),
            emoji: (!emoji.is_empty()).then(|| emoji.to_owned()),
            audio_base64: STANDARD.encode(bytes),
        });
        if let Some(input) = input.get_untracked() {
            input.set_value("");
        }
    };
    view! {
        <form class="settings-card" on:submit=move |e: leptos::ev::SubmitEvent| { e.prevent_default(); if ready() { upload(); } }>
            <h3>{move || crate::i18n::t("Add a sound")}</h3>
            <p class="settings-hint">{move || crate::i18n::t("Upload WAV, MP3, Ogg Vorbis or FLAC up to 1 MiB. Sounds are trimmed to 5 seconds and their loudness is evened out.")}</p>
            <fieldset class="space-y-4" disabled=move || ui.busy.get() || state.get().is_none()>
                <label class="settings-field"><span>{move || crate::i18n::t("Sound file")}</span>
                    <input node_ref=input type="file" accept="audio/*,.wav,.mp3,.ogg,.oga,.flac" on:change=choose/></label>
                <label class="settings-field"><span>{move || crate::i18n::t("Name")}</span>
                    <input type="text" maxlength=MAX_NAME_CHARS.to_string() prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/></label>
                <label class="settings-field"><span>{move || crate::i18n::t("Emoji (optional)")}</span>
                    <input type="text" maxlength="8" placeholder="🦆" prop:value=move || emoji.get() on:input=move |e| emoji.set(event_target_value(&e))/></label>
                <div class="settings-actions"><button class="settings-primary" disabled=move || !ready()>{move || crate::i18n::t("Upload sound")}</button></div>
            </fieldset>
        </form>
        <section class="settings-card">
            <h3>{move || crate::i18n::t("Server sounds")}</h3>
            <p class="settings-hint">{move || state.get().map(|s| thiscord_frontend::localized_format!(crate::i18n::language(), "{count} of {limit} sounds used. Control who can see and play them with the Use soundboard permission in Roles or Channels & permissions.", count = uploaded(&s), limit = s.limit))}</p>
            <p class="settings-hint">{move || crate::i18n::t("Built-in sounds are available in every server and do not count toward the limit.")}</p>
            <ul class="settings-device-list">{move || state.get().map(|current| current.sounds.into_iter().map(|sound| {
                let id = sound.id;
                let revision = current.revision;
                view! {<li>
                    <div class="settings-device-icon" aria-hidden="true">{sound.emoji.clone().map_or_else(|| view! {<Icon kind=Glyph::Soundboard/>}.into_any(), |emoji| view! {<span class="text-xl">{emoji}</span>}.into_any())}</div>
                    <div class="min-w-0 flex-1"><strong class="block truncate">{label(&sound)}</strong><p>{format!("{:.1} s", f64::from(sound.duration_ms) / 1000.0)}</p></div>
                    {if sound.builtin {
                        view! {<span class="settings-badge">{move || crate::i18n::t("Built-in")}</span>}.into_any()
                    } else {
                        view! {<ConfirmButton ui=ui label="Delete" title="Delete this sound?" detail="Everyone in this server will lose access to it." on_confirm=Callback::new(move |()| run(SoundboardRequest::Delete { guild_id: guild, revision, sound_id: id }))/>}.into_any()
                    }}
                </li>}
            }).collect_view())}</ul>
        </section>
    }
}
