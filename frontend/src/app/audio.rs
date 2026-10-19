use super::Ui;
use super::icons::{Glyph, Icon};
use super::shortcuts::ShortcutSettings;
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::{ChannelId, GuildId, audio::*, voice::VoiceStatus};

#[derive(Clone)]
pub(super) struct VoiceLocation {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub guild_name: String,
}

pub(super) fn join(
    ui: Ui,
    guild: thiscord_shared::permissions::Guild,
    channel: thiscord_shared::permissions::Channel,
) {
    if ui.voice.get_untracked().channel_id == Some(channel.id) {
        ui.selected_channel.set(Some(channel));
        ui.page.set("servers");
        return;
    }
    super::friends::end_call(ui);
    join_location(
        ui,
        VoiceLocation {
            guild_id: guild.id,
            channel_id: channel.id,
            channel_name: channel.name,
            guild_name: guild.name,
        },
    );
}

pub(super) fn join_location(ui: Ui, location: VoiceLocation) {
    if !desktop() && !ui.audio_control.is_remote() {
        ui.status.set("Open the desktop app to join voice".into());
        return;
    }
    if ui.voice_joining.get_untracked()
        || ui.voice.get_untracked().channel_id == Some(location.channel_id)
    {
        return;
    }
    let Some(token) = ui.token.get_untracked() else {
        return;
    };
    ui.voice_joining.set(true);
    let settings = ui.audio.get_untracked();
    leptos::task::spawn_local(async move {
        let result = ui.audio_control.invoke::<()>("voice_join", json!({"token":token,"guildId":location.guild_id,"channelId":location.channel_id,"settings":settings})).await;
        ui.voice_joining.set(false);
        if ui.token.get_untracked().as_deref() != Some(&token) {
            return;
        }
        match result {
            Ok(()) => {
                ui.voice_location.set(Some(location));
                if let Ok(status) = ui
                    .audio_control
                    .invoke::<VoiceStatus>("voice_status", json!({}))
                    .await
                {
                    ui.voice.set(status);
                }
            }
            Err(error) => ui.status.set(error),
        }
    });
}

pub(super) fn save(ui: Ui) {
    ui.audio_revision.update(|r| *r = r.wrapping_add(1));
    if ui.audio_saving.get_untracked() {
        return; // The save pump picks up the newest controls after this change.
    }
    ui.audio_saving.set(true);
    leptos::task::spawn_local(async move {
        loop {
            let Some(revision) = ui.audio_revision.try_get_untracked() else {
                return;
            };
            let settings = ui.audio.get_untracked();
            if let Err(error) = ui
                .audio_control
                .invoke::<()>("audio_save", json!({"settings":settings}))
                .await
            {
                if ui.status.is_disposed() {
                    return;
                }
                ui.status.set(error);
                if let Ok(saved) = ui
                    .audio_control
                    .invoke::<AudioSettings>("audio_current", json!({}))
                    .await
                    && !ui.audio_revision.is_disposed()
                    && (ui.audio_control.is_remote()
                        || ui.audio_revision.get_untracked() == revision)
                {
                    ui.audio_control.observe(&saved);
                    ui.audio.set(saved);
                }
                if ui.audio_control.is_remote() {
                    if !ui.audio_saving.is_disposed() {
                        ui.audio_saving.set(false);
                    }
                    return;
                }
            }
            if ui.audio_revision.is_disposed() {
                return;
            }
            if ui.audio_control.is_remote()
                && let Ok(saved) = ui
                    .audio_control
                    .invoke::<AudioSettings>("audio_current", json!({}))
                    .await
            {
                if ui.audio.is_disposed() {
                    return;
                }
                ui.audio_control.observe(&saved);
                if ui.audio_revision.get_untracked() == revision {
                    ui.audio.set(saved);
                }
            }
            if ui.audio_revision.get_untracked() == revision {
                break;
            }
        }
        ui.audio_saving.set(false);
    });
}
fn pressed(pressed: bool) {
    leptos::task::spawn_local(async move {
        let _ = native::<AudioStatus>("audio_pressed", json!({"pressed":pressed})).await;
    });
}
pub(super) fn leave(ui: Ui) {
    super::friends::end_call(ui);
    leptos::task::spawn_local(async move {
        if let Err(error) = ui
            .audio_control
            .invoke::<()>("voice_leave", json!({}))
            .await
        {
            ui.status.set(error);
        }
    });
}
#[component]
pub(super) fn AudioHost(ui: Ui) -> impl IntoView {
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || {
        abort.abort();
        if desktop() {
            leptos::task::spawn_local(async move {
                let _ = ui
                    .audio_control
                    .invoke::<()>("voice_leave", json!({}))
                    .await;
                let _ = ui
                    .audio_control
                    .invoke::<AudioStatus>("audio_stop", json!({}))
                    .await;
            });
        }
    });
    if desktop() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    match ui
                        .audio_control
                        .invoke::<AudioSettings>("audio_load", json!({}))
                        .await
                    {
                        Ok(settings) => ui.audio.set(settings),
                        Err(error) => ui.status.set(error),
                    }
                    loop {
                        if let Ok(status) = ui
                            .audio_control
                            .invoke::<VoiceStatus>("voice_status", json!({}))
                            .await
                        {
                            if ui.voice.get_untracked().channel_id.is_some()
                                && status.channel_id.is_none()
                            {
                                ui.status.set(status.message.clone());
                            }
                            ui.voice.set(status);
                        }
                        if let Ok(status) = ui
                            .audio_control
                            .invoke::<AudioStatus>("audio_status", json!({}))
                            .await
                        {
                            ui.audio_status.set(Some(status));
                        }
                        if let Ok(settings) = ui
                            .audio_control
                            .invoke::<AudioSettings>("audio_current", json!({}))
                            .await
                            && !ui.audio_saving.get_untracked()
                        {
                            ui.audio.set(settings);
                        }
                        gloo_timers::future::TimeoutFuture::new(500).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
}
pub(super) fn toggle(ui: Ui, action: AudioToggle) {
    leptos::task::spawn_local(async move {
        let result = ui
            .audio_control
            .invoke::<()>("audio_toggle", json!({"action":action}))
            .await;
        if ui.status.is_disposed() {
            return;
        }
        if let Err(error) = result {
            ui.status.set(error);
        }
        if let Ok(settings) = ui
            .audio_control
            .invoke::<AudioSettings>("audio_current", json!({}))
            .await
            && !ui.audio.is_disposed()
        {
            ui.audio.update(|current| {
                current.muted = settings.muted;
                current.deafened = settings.deafened;
            });
        }
    });
}
#[component]
fn VoiceControls(ui: Ui) -> impl IntoView {
    let available = move || desktop() || ui.audio_control.is_remote();
    view! {<div class="flex shrink-0 items-center">
        <button class="icon-button" disabled=move ||!available() aria-label=move ||if ui.audio.get().muted {"Unmute"}else{"Mute"} data-tip=move ||if !available(){"Microphone requires desktop"}else if ui.audio.get().muted{"Unmute"}else{"Mute"} aria-pressed=move || ui.audio.get().muted on:click=move |_|toggle(ui, AudioToggle::Mute)>{move||view!{<Icon kind=if ui.audio.get().muted{Glyph::MicOff}else{Glyph::Mic}/>}}</button>
        <button class="icon-button" disabled=move ||!available() aria-label=move ||if ui.audio.get().deafened {"Undeafen"}else{"Deafen"} data-tip=move ||if !available(){"Audio requires desktop"}else if ui.audio.get().deafened{"Undeafen"}else{"Deafen"} aria-pressed=move || ui.audio.get().deafened on:click=move |_|toggle(ui, AudioToggle::Deafen)>{move||view!{<Icon kind=if ui.audio.get().deafened{Glyph::Deafen}else{Glyph::Headphones}/>}}</button>
    </div>}
}
#[component]
pub(super) fn VoiceBar(ui: Ui) -> impl IntoView {
    let location = move || {
        let channel = ui.voice.get().channel_id;
        ui.voice_location
            .get()
            .filter(|location| Some(location.channel_id) == channel)
            .map(|location| format!("{} / {}", location.channel_name, location.guild_name))
            .or_else(|| {
                ui.server.get().and_then(|home| {
                    home.channels
                        .iter()
                        .find(|c| Some(c.id) == channel)
                        .map(|c| format!("{} / {}", c.name, home.guild.name))
                })
            })
            .unwrap_or_else(|| "Voice channel".into())
    };
    view! {<aside class="user-dock" aria-label="Your account and voice controls">
        <Show when=move||ui.voice.get().channel_id.is_some()><div class="voice-dock">
            <div class="flex items-center gap-2">
                <span class="text-emerald-400"><Icon kind=Glyph::Signal/></span>
                <div class="min-w-0 flex-1"><button class="voice-health text-sm font-semibold" aria-label="Voice connection diagnostics" data-tip=move ||if ui.voice.get().connected {thiscord_frontend::audio_quality::brief(ui.audio_status.get().and_then(|s|s.quality).as_ref())}else{"Connecting · diagnostics pending".into()} class:text-emerald-400=move ||ui.voice.get().connected class:text-amber-300=move ||!ui.voice.get().connected role="status">{move ||if ui.voice.get().connected{"Voice connected"}else{"Connecting to voice…"}}</button><button class="block max-w-full truncate text-left text-xs text-white/55 hover:text-white" data-tip="Open call controls" aria-label="Open call controls" on:click=move |_|super::servers::open_voice(ui)>{location}</button></div>
                <button class="icon-button danger" aria-label="Disconnect voice" data-tip="Disconnect" disabled=move ||ui.voice_joining.get() on:click=move |_|leave(ui)><Icon kind=Glyph::Disconnect/></button>
            </div>
            <div class="mt-2 flex items-center gap-2">
                <super::camera::CameraControls ui=ui/>
                <super::screen::ScreenControls ui=ui/>
                <button class="icon-button voice-action" aria-label="Audio settings" data-tip="Audio settings" on:click=move |_|ui.page.set("audio")><Icon kind=Glyph::Volume/></button>
                <Show when=move||!ui.audio_control.is_remote() && ui.audio.get().mode==TransmitMode::PushToTalk><button class="icon-button voice-action touch-none select-none" aria-label="Hold to talk" data-tip="Hold to talk" on:pointerdown=move |_|pressed(true) on:pointerup=move |_|pressed(false) on:pointerleave=move |_|pressed(false) on:pointercancel=move |_|pressed(false) on:keydown=move|e|{if matches!(e.key().as_str()," "|"Enter"){e.prevent_default();pressed(true);}} on:keyup=move|_|pressed(false) on:blur=move |_|pressed(false)><Icon kind=Glyph::Mic/></button></Show>
            </div>
        <Show when=move||ui.audio_status.get().and_then(|s|s.recording).is_some_and(|r|r.active)>
            <button class="mt-2 rounded bg-red-700 px-3 py-1 text-xs text-white" on:click=move |_|debug_command(ui,"audio_debug_stop")>
                {move||format!("● Recording {}s · Stop",ui.audio_status.get().and_then(|s|s.recording).map_or(0,|r|r.elapsed_ms/1000))}
            </button>
        </Show>
        </div></Show>
        <div class="flex min-w-0 items-center gap-2 p-2">
            <button class="flex min-w-0 flex-1 items-center gap-2 rounded-lg p-1 text-left hover:bg-white/5" aria-label="Your profile" data-tip=move ||ui.account.get().map(|a|format!("{} (@{})",a.display_name,a.username)).unwrap_or_default() on:click=move |_|ui.page.set("profile")>
                <span class="relative shrink-0"><crate::avatar::Avatar name=Signal::derive(move ||ui.account.get().map(|a|a.display_name).unwrap_or_default()) avatar_id=Signal::derive(move ||ui.account.get().and_then(|a|a.avatar_id))/><span class="connection-dot" class:connected=move ||ui.socket_status.get()==super::chat::ConnectionStatus::Connected class:connecting=move ||matches!(ui.socket_status.get(),super::chat::ConnectionStatus::Connecting|super::chat::ConnectionStatus::Reconnecting)></span></span>
                <span class="min-w-0"><span class="block truncate text-sm font-semibold">{move ||ui.account.get().map(|a|if a.display_name.is_empty(){a.username}else{a.display_name})}</span><span class="block truncate text-xs text-white/55" role="status">{move ||ui.socket_status.get().label()}</span></span>
            </button>
            <VoiceControls ui=ui/>
            <button class="icon-button shrink-0" aria-label="Settings" data-tip="Settings" on:click=move |_|ui.page.set("profile")><Icon kind=Glyph::Settings/></button>
        </div>
    </aside>}
}
fn debug_command(ui: Ui, command: &'static str) {
    leptos::task::spawn_local(async move {
        match ui
            .audio_control
            .invoke::<AudioStatus>(command, json!({}))
            .await
        {
            Ok(status) => ui.audio_status.set(Some(status)),
            Err(error) => ui.status.set(error),
        }
    });
}
#[component]
fn DebugRecording(ui: Ui) -> impl IntoView {
    view! {<section class="space-y-2 rounded-lg border border-white/15 p-4" aria-label="Echo debug recording">
        <h3 class="font-semibold">"Echo debug recording"</h3>
        <p class="text-sm text-white/70">"Save up to 60 seconds of speaker output, raw microphone, and outgoing audio for troubleshooting. Raw microphone is recorded even while muted. Tell the other participants before starting. Files stay on this computer until you choose to share them."</p>
        <button class="rounded bg-brand px-3 py-2 text-sm disabled:opacity-50"
            disabled=move||ui.voice.get().channel_id.is_none() || ui.audio_status.get().is_none_or(|s|!s.running || s.recording.is_some_and(|r|r.active || r.saving))
            on:click=move |_|debug_command(ui,"audio_debug_start")>"Start debug recording"</button>
        <Show when=move||ui.audio_status.get().and_then(|s|s.recording).is_some()>
            <p class="text-sm" role="status">{move||ui.audio_status.get().and_then(|s|s.recording).map(|r| {
                if let Some(error)=r.error { error }
                else if r.active { format!("● Recording — {} / 60 seconds",r.elapsed_ms/1000) }
                else if r.saving { "Saving recording…".into() }
                else { format!("Saved to {}. Share the entire folder, including timing metadata.",r.directory) }
            })}</p>
            <Show when=move||ui.audio_status.get().and_then(|s|s.recording).is_some_and(|r|r.active)>
                <button class="rounded bg-red-700 px-3 py-2 text-sm" on:click=move |_|debug_command(ui,"audio_debug_stop")>"Stop and save"</button>
            </Show>
            <button class="text-sm underline disabled:opacity-50" disabled=move||ui.audio_status.get().and_then(|s|s.recording).is_none_or(|r|r.active||r.saving)
                on:click=move |_|leptos::task::spawn_local(async move {if let Err(error)=ui.audio_control.invoke::<()>("audio_debug_folder",json!({})).await{ui.status.set(error);}})>"Open recording folder"</button>
        </Show>
        <p class="text-xs text-white/60">"Join voice first. For a useful sample, have one person speak for 5–10 seconds, then talk at the same time. Delete the recording when it is no longer needed."</p>
    </section>}
}
#[component]
fn StreamVolumes(ui: Ui) -> impl IntoView {
    view! {<div class="space-y-3"><For each=move || { ui.audio_status.get().map(|s| s.streams).unwrap_or_default().into_iter().filter(|s| !s.target.is_some_and(|t| t.shared_audio)).collect::<Vec<_>>() } key=|s| (s.id.clone(), s.target) children=move |stream| view! { <StreamVolume ui=ui stream=stream/> }/></div>}
}

#[component]
pub(super) fn StreamVolume(
    ui: Ui,
    stream: StreamLevel,
    #[prop(default = false)] compact: bool,
) -> impl IntoView {
    let id = stream.id.parse::<usize>().unwrap_or(0);
    let target = stream.target;
    let shared_audio = target.is_some_and(|t| t.shared_audio);
    let gain = RwSignal::new((stream.volume * 100.0).round());
    let restore = RwSignal::new(if stream.volume > 0.0 {
        stream.volume
    } else {
        1.0
    });
    let busy = RwSignal::new(false);
    let dragging = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let current_volume = Memo::new(move |_| {
        ui.audio_status.get().and_then(|s| {
            s.streams
                .into_iter()
                .find(|s| s.id == id.to_string() && s.target == target)
                .map(|s| s.volume)
        })
    });
    Effect::new(move |_| {
        if let Some(current) = current_volume.get()
            && !dragging.get_untracked()
            && !busy.get_untracked()
        {
            gain.set((current * 100.0).round());
        }
    });
    let save = move |value: f32| {
        dragging.set(false);
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = ui
                .audio_control
                .invoke::<AudioStatus>(
                    "audio_volume",
                    json!({"stream":id,"gain":value,"target":target}),
                )
                .await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(status) => ui.audio_status.set(Some(status)),
                Err(message) => {
                    error.set(message);
                    if let Some(current) = current_volume.get_untracked() {
                        gain.set((current * 100.0).round());
                    }
                }
            }
            busy.set(false);
        });
    };
    view! {
        <div class="stream-volume" class:compact=compact>
            <label class="flex flex-wrap items-center gap-3">
                <span class="volume-label">{if shared_audio { "Stream audio".to_owned() } else if compact {"Voice volume".to_owned()} else { stream.label }}</span>
                <input aria-label=if shared_audio { "Screen share volume" } else { "Speaker volume" } type="range" min="0" max="200" step="1"
                    disabled=move || busy.get() prop:value=move || gain.get().to_string()
                    on:input=move |e| { if let Ok(value) = event_target_value(&e).parse::<f32>() { dragging.set(true);gain.set(value); } }
                    on:change=move |e| { if let Ok(value) = event_target_value(&e).parse::<f32>() { save(value / 100.0); } }/>
                <output class="volume-value">{move || format!("{}%", gain.get())}</output>
            </label>
            {shared_audio.then(|| view! {
                <button class="icon-button" disabled=move || busy.get()
                    aria-label=move ||if gain.get()==0.0{"Unmute stream audio"}else{"Mute stream audio"}
                    data-tip=move ||if gain.get()==0.0{"Unmute stream audio"}else{"Mute stream audio"}
                    aria-pressed=move || (gain.get() == 0.0).to_string()
                    on:click=move |_| {
                        if gain.get_untracked() > 0.0 {
                            restore.set(gain.get_untracked() / 100.0);
                            save(0.0);
                        } else { save(restore.get_untracked()); }
                    }>{move ||view!{<Icon kind=if gain.get()==0.0{Glyph::VolumeOff}else{Glyph::Volume}/ >}}</button>
            })}
            <Show when=move || !error.get().is_empty()><p class="w-full text-sm text-red-300" role="alert">{move || error.get()}</p></Show>
        </div>
    }
}
#[component]
pub(super) fn VoiceChannel(
    ui: Ui,
    guild: GuildId,
    channel: ChannelId,
    name: String,
) -> impl IntoView {
    view! {<section class="voice-channel-view">
        <header class="voice-channel-header"><Icon kind=Glyph::Volume/><h2>{name}</h2></header>
        <Show when=move ||desktop() || ui.audio_control.is_remote() fallback=||view!{<p class="voice-channel-empty">"Open the desktop app to use voice and watch streams."</p>}>
            <super::screen::ScreenViewer ui=ui guild=guild channel=channel/>
        </Show>
    </section>}
}

#[component]
pub(super) fn AudioSettingsPanel(ui: Ui) -> impl IntoView {
    let devices = RwSignal::new(Vec::<AudioDevice>::new());
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || {
        abort.abort();
        if desktop()
            && !ui.audio_control.is_remote()
            && ui.voice.get_untracked().channel_id.is_none()
        {
            leptos::task::spawn_local(async move {
                let _ = ui
                    .audio_control
                    .invoke::<AudioStatus>("audio_stop", json!({}))
                    .await;
            });
        }
    });
    if desktop() || ui.audio_control.is_remote() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    loop {
                        match ui
                            .audio_control
                            .invoke::<Vec<AudioDevice>>("audio_devices", json!({}))
                            .await
                        {
                            Ok(list) => devices.set(list),
                            Err(error) => message.set(error),
                        }
                        gloo_timers::future::TimeoutFuture::new(3000).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
    let test = move |microphone| {
        busy.set(true);
        let settings = ui.audio.get_untracked();
        leptos::task::spawn_local(async move {
            let result = ui
                .audio_control
                .invoke::<AudioStatus>(
                    "audio_test",
                    json!({"settings":settings,"microphone":microphone}),
                )
                .await;
            if busy.try_get_untracked().is_none() {
                return;
            }
            busy.set(false);
            match result {
                Ok(status) => ui.audio_status.set(Some(status)),
                Err(error) => message.set(error),
            }
        });
    };
    view! {<div class="space-y-5"><h2 class="settings-panel-title text-xl font-semibold">"Audio & voice"</h2>
            <Show when=move ||ui.audio_control.is_remote()><ShortcutSettings control=ui.audio_control/><crate::overlay::OverlaySettings control=ui.audio_control/></Show>
            <Show when=move ||desktop() || ui.audio_control.is_remote() fallback=||view!{<p>"Audio devices and voice are available in the desktop app."</p>}>
                <section class="settings-card"><h3>"Devices & microphone"</h3><p class="settings-hint">"Changes save automatically and apply to your current call."</p>
                {[(true,"Microphone"),(false,"Speakers / headphones")].into_iter().map(move|(input,label)|view!{
                    <label class="block space-y-2"><span>{label}</span><select class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" disabled=move||ui.audio_saving.get() prop:value=move||{let s=ui.audio.get();if input{s.input}else{s.output}.unwrap_or_default()} on:change=move|e|{let v=event_target_value(&e);ui.audio.update(|s|{let value=(!v.is_empty()).then_some(v);if input{s.input=value;}else{s.output=value;}});save(ui);}>
                        <option value="">"System default"</option>{move||devices.get().into_iter().filter(|d|d.input==input).map(|d|view!{<option value=d.id>{d.name}</option>}).collect_view()}
                    </select></label>
                }).collect_view()}
                <label class="flex flex-wrap gap-3"><span>"Output volume"</span><output class="text-xs text-white/60">{move ||format!("{}%",(ui.audio.get().output_volume*100.0).round())}</output><input type="range" min="0" max="200" prop:value=move||(ui.audio.get().output_volume*100.0).to_string() on:change=move|e|{if let Ok(v)=event_target_value(&e).parse::<f32>(){ui.audio.update(|s|s.output_volume=v/100.0);save(ui);}}/></label>
                <label class="block space-y-2"><span>"Transmit mode"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move||if ui.audio.get().mode==TransmitMode::PushToTalk{"ptt"}else{"activity"} on:change=move|e|{ui.audio.update(|s|s.mode=if event_target_value(&e)=="ptt"{TransmitMode::PushToTalk}else{TransmitMode::VoiceActivity});save(ui);}><option value="activity">"Voice activation"</option><option value="ptt">"Push to talk"</option></select></label>
                <label class="flex flex-wrap gap-3"><span>"Microphone boost"</span><output class="text-xs text-white/60">{move||format!("{:.1}×",ui.audio.get().input_gain)}</output><input type="range" min="1" max="10" step="0.1" disabled=move||ui.audio_saving.get() prop:value=move||ui.audio.get().input_gain.to_string() on:change=move|e|{if let Ok(v)=event_target_value(&e).parse::<f32>(){ui.audio.update(|s|s.input_gain=v);save(ui);}}/></label>
                <p class="settings-hint">"Boosts your microphone before voice activation and automatic gain. Start at 2× for a quiet microphone; reduce it if your voice distorts or background noise activates it. 1× leaves the level unchanged."</p>
                <label class="flex flex-wrap gap-3"><span>"Activation threshold"</span><input type="range" min="1" max="100" prop:value=move||(ui.audio.get().activation_threshold*1000.0).to_string() on:change=move|e|{if let Ok(v)=event_target_value(&e).parse::<f32>(){ui.audio.update(|s|s.activation_threshold=v/1000.0);save(ui);}}/></label>
                <label class="flex items-center gap-3"><input type="checkbox" disabled=move||ui.audio_saving.get() prop:checked=move||ui.audio.get().global_push_to_talk on:change=move|e|{ui.audio.update(|s|s.global_push_to_talk=event_target_checked(&e));save(ui);}/>"Global push-to-talk: Ctrl+Shift+Space (Windows, macOS, Linux/X11)"</label>
                <div class="settings-actions">                <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||busy.get()||ui.voice.get().channel_id.is_some() on:click=move |_|test(false)>"Test playback (5 seconds)"</button>
                    <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-50" disabled=move||busy.get()||ui.voice.get().channel_id.is_some() on:click=move |_|test(true)>"Test microphone (60 seconds)"</button>
    </div><p class="settings-hint">"Use headphones for the microphone test. Sound tests are available when you’re outside a call."</p>
                </section>
                <section class="settings-card"><h3>"Voice processing"</h3>
                    <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().noise_suppression on:change=move|e|{ui.audio.update(|s|s.noise_suppression=event_target_checked(&e));save(ui);}/>"Noise suppression"</label>
                    <details><summary>"Noise suppression model"</summary><label class="block space-y-2"><span>"Model"</span><select class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" disabled=move||ui.audio_saving.get() prop:value=move||match ui.audio.get().noise_suppression_model { NoiseSuppressionModel::Sonora=>"sonora", NoiseSuppressionModel::DeepFilterNet3=>"deep_filter_net3" } on:change=move|e|{ui.audio.update(|s|s.noise_suppression_model=if event_target_value(&e)=="deep_filter_net3" {NoiseSuppressionModel::DeepFilterNet3}else{NoiseSuppressionModel::Sonora});save(ui);}>
                        <option value="sonora">"Standard (Sonora)"</option><option value="deep_filter_net3">"DeepFilterNet3 (experimental)"</option>
                    </select></label>
                    <p class="text-sm text-white/60">"Models can be changed during a call. Echo cancellation needs time to adapt after a change. DeepFilterNet3 uses more CPU and adds buffering; test it with your microphone. It reduces noise but may preserve other people's voices."</p>
                    </details>
                    <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().automatic_gain on:change=move|e|{ui.audio.update(|s|s.automatic_gain=event_target_checked(&e));save(ui);}/>"Automatic microphone gain"</label>
                    <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().echo_cancellation on:change=move|e|{ui.audio.update(|s|s.echo_cancellation=event_target_checked(&e));save(ui);}/>"Echo cancellation"</label>
                    <details><summary>"Experimental echo processing"</summary><label class="flex gap-3"><input type="checkbox" disabled=move||ui.audio_saving.get() prop:checked=move||ui.audio.get().neural_echo on:change=move|e|{ui.audio.update(|s|s.neural_echo=event_target_checked(&e));save(ui);}/>"Neural residual echo estimation (experimental)"</label>
                    <Show when=move||ui.audio.get().neural_echo>
                        <details><summary class="cursor-pointer text-sm text-white/60">"Advanced model settings"</summary><label class="mt-2 block space-y-2"><span>"Model file override (optional)"</span><input type="text" class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" placeholder="Bundled REE v2 model" disabled=move||ui.audio_saving.get() prop:value=move||ui.audio.get().neural_echo_model.unwrap_or_default() on:change=move|e|{let value=event_target_value(&e);ui.audio.update(|s|s.neural_echo_model=(!value.trim().is_empty()).then_some(value));save(ui);}/></label></details>
                        <p class="text-sm text-slate-400">"Uses the bundled model when the override is empty. Enable echo cancellation too. Changes apply without leaving voice."</p>
                    </Show>
                    </details>
                </section>
                <Show when=move||ui.audio_saving.get()><p class="text-sm text-white/60" role="status">"Applying audio settings..."</p></Show>
                <VoiceControls ui=ui/>
                <details class="settings-card"><summary>"Advanced & troubleshooting"</summary><div class="space-y-5"><p class="settings-hint">"During calls, instance administrators can adjust audio settings and request technical diagnostics for support."</p>
                <label class="flex gap-3"><input type="checkbox" disabled=move||ui.audio_saving.get() prop:checked=move||ui.audio.get().adaptive_fec on:change=move|e|{ui.audio.update(|s|s.adaptive_fec=event_target_checked(&e));save(ui);}/>"Adaptive Opus in-band FEC"</label>
                <p class="settings-hint">"Adjusts microphone loss protection from server receiver reports during your call. Keeps the 32 kbit/s codec budget; more protection can reduce speech detail. Adds no buffering delay. Uses the standard 5% hint while feedback is unavailable or stale; burst losses may remain audible."</p>
                <Show when=move||ui.audio_control.is_remote() && ui.audio_status.get().is_some_and(|s|s.quality.is_none_or(|q|q.adaptive_fec.is_none()))><p class="settings-hint">"Adaptive FEC status is unavailable on this client. Update the remote desktop app if it does not support or retain this setting."</p></Show>
                <Show when=move || !ui.audio_control.is_remote()><DebugRecording ui=ui/></Show>
                <super::diagnostics::DiagnosticsSettings control=ui.audio_control/>
                <label class="block">"Raw microphone"<meter class="ml-3 w-48" min="0" max="1" value=move||ui.audio_status.get().map(|s|s.raw_input_level).unwrap_or(0.0) /></label>
                <label class="block">"After noise / echo processing"<meter class="ml-3 w-48" min="0" max="1" value=move||ui.audio_status.get().map(|s|s.input_level).unwrap_or(0.0) /></label>
                <p class="text-sm" role="status">{move||ui.audio_status.get().map(|s|format!("{} · processing resets {}",s.message,s.processing_resets))}</p>
                <p class="text-sm text-slate-300">{move||thiscord_frontend::audio_quality::summary(ui.audio_status.get().and_then(|s|s.quality).as_ref())}</p>
                <details class="rounded border border-white/10 p-3 text-sm">
                    <summary class="cursor-pointer">"Advanced voice quality diagnostics"</summary>
                    <pre class="mt-3 max-h-96 overflow-auto whitespace-pre-wrap text-xs text-slate-300">{move||thiscord_frontend::audio_quality::detail(ui.audio_status.get().and_then(|s|s.quality).as_ref())}</pre>
                </details>
                <details class="space-y-2 text-sm text-white/70">
                    <summary class="cursor-pointer">"Echo diagnostics"</summary>
                    <p>{move||ui.audio_status.get().and_then(|s|s.echo).map(|d|format!(
                        "Playback reference: {:.1}% · Filter estimate: {} · Estimated delay: {} · Input clipping: {:.2}% · Automatic gain: {}",
                        d.reference_level*100.0,
                        d.filter_reduction_db.map(|v|format!("{v:.1} dB")).unwrap_or_else(||"warming up / no playback".into()),
                        d.estimated_delay_ms.map(|v|format!("{v} ms")).unwrap_or_else(||"unavailable".into()),
                        d.clipped_input_percent,if d.automatic_gain{"on"}else{"off"}
                    )).unwrap_or_else(||"Join voice and enable echo cancellation to view diagnostics.".into())}</p>
                    <p>"The filter estimate is not a measurement of audible echo. Compare these values while only the other person speaks, then while both of you speak. Rising processing resets indicate lost audio blocks. Repeated input clipping suggests lowering microphone gain."</p>
                </details>
                <p class="text-sm text-white/60">"Allow microphone access in your OS privacy settings. Use headphones for the microphone test. Echo cancellation removes Thiscord playback from your microphone; it cannot remove sound from other apps. Allow a few seconds for it to adapt. Compare the meters while someone else speaks and you stay quiet."</p>
                <Show when=move || !ui.audio_control.is_remote()><div class="flex flex-wrap gap-3">
                    <button class="underline" on:click=move |_|{leave(ui);leptos::task::spawn_local(async move{let _=ui.audio_control.invoke::<AudioStatus>("audio_stop",json!({})).await;});}>"Stop audio"</button>
                    <button class="underline disabled:opacity-50" disabled=move||busy.get() on:click=move |_|{busy.set(true);message.set("Testing encrypted WebRTC forwarding…".into());leptos::task::spawn_local(async move{let result=ui.audio_control.invoke::<String>("audio_webrtc_probe",json!({})).await;if busy.try_get_untracked().is_some(){busy.set(false);message.set(result.unwrap_or_else(|e|e));}});}>"Test WebRTC"</button>
                </div></Show>
                <StreamVolumes ui=ui/><super::screen::ShareDiagnostics/><p role="status" class="text-sm text-white/70">{move||message.get()}</p></div></details>
            </Show>
        </div>}
}
