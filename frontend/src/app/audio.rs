use super::Ui;
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::{ChannelId, GuildId, audio::*, voice::VoiceStatus};

pub(super) fn save(ui: Ui) {
    ui.audio_revision.update(|r| *r = r.wrapping_add(1));
    let settings = ui.audio.get_untracked();
    if settings.muted || settings.deafened {
        leptos::task::spawn_local(async move {
            let _ = native::<()>("audio_restrict", json!({"settings":settings})).await;
        });
    }
    if ui.audio_saving.get_untracked() {
        return; // The save pump picks up the newest controls after this change.
    }
    ui.audio_saving.set(true);
    leptos::task::spawn_local(async move {
        loop {
            let revision = ui.audio_revision.get_untracked();
            let settings = ui.audio.get_untracked();
            if let Err(error) = native::<()>("audio_save", json!({"settings":settings})).await {
                ui.status.set(error);
                if let Ok(mut saved) = native::<AudioSettings>("audio_current", json!({})).await
                    && ui.audio_revision.get_untracked() == revision
                {
                    // A failed device/model change must not visually undo an
                    // urgent mute/deafen that already took effect.
                    saved.muted |= settings.muted;
                    saved.deafened |= settings.deafened;
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
    leptos::task::spawn_local(async move {
        if let Err(error) = native::<()>("voice_leave", json!({})).await {
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
            leptos::task::spawn_local(async {
                let _ = native::<()>("voice_leave", json!({})).await;
                let _ = native::<AudioStatus>("audio_stop", json!({})).await;
            });
        }
    });
    if desktop() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    match native::<AudioSettings>("audio_load", json!({})).await {
                        Ok(settings) => ui.audio.set(settings),
                        Err(error) => ui.status.set(error),
                    }
                    loop {
                        if let Ok(status) = native::<VoiceStatus>("voice_status", json!({})).await {
                            if ui.voice.get_untracked().channel_id.is_some()
                                && status.channel_id.is_none()
                            {
                                ui.status.set(status.message.clone());
                            }
                            ui.voice.set(status);
                        }
                        if let Ok(status) = native::<AudioStatus>("audio_status", json!({})).await {
                            ui.audio_status.set(Some(status));
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
#[component]
fn VoiceControls(ui: Ui) -> impl IntoView {
    view! {<div class="flex flex-wrap items-center gap-3">
        <button class="rounded bg-white/10 px-3 py-2 text-sm" on:click=move |_|{ui.audio.update(|s|s.muted = !s.muted);save(ui);}>{move||if ui.audio.get().muted{"Unmute"}else{"Mute"}}</button>
        <button class="rounded bg-white/10 px-3 py-2 text-sm" on:click=move |_|{ui.audio.update(|s|s.deafened = !s.deafened);save(ui);}>{move||if ui.audio.get().deafened{"Undeafen"}else{"Deafen"}}</button>
        <Show when=move||ui.audio.get().mode==TransmitMode::PushToTalk><button class="touch-none select-none rounded bg-brand px-3 py-2 text-sm" on:pointerdown=move |_|pressed(true) on:pointerup=move |_|pressed(false) on:pointerleave=move |_|pressed(false) on:pointercancel=move |_|pressed(false) on:keydown=move|e|{if matches!(e.key().as_str()," "|"Enter"){e.prevent_default();pressed(true);}} on:keyup=move|_|pressed(false) on:blur=move |_|pressed(false)>"Hold to talk"</button></Show>
    </div>}
}
#[component]
pub(super) fn VoiceBar(ui: Ui) -> impl IntoView {
    view! {<Show when=move||ui.voice.get().channel_id.is_some()><aside class="flex shrink-0 flex-wrap items-center justify-between gap-3 rounded-lg bg-black/20 p-3" aria-label="Voice controls">
        <span class="text-sm">{move||ui.voice.get().message}</span><VoiceControls ui=ui/><super::screen::ScreenControls ui=ui/>
        <Show when=move||ui.audio_status.get().and_then(|s|s.recording).is_some_and(|r|r.active)>
            <button class="rounded bg-red-700 px-3 py-2 text-sm text-white" on:click=move |_|debug_command(ui,"audio_debug_stop")>
                {move||format!("● Recording {}s · Stop",ui.audio_status.get().and_then(|s|s.recording).map_or(0,|r|r.elapsed_ms/1000))}
            </button>
        </Show>
        <button class="text-sm underline" on:click=move |_|ui.page.set("audio")>"Audio settings"</button>
        <button class="text-sm text-red-300" on:click=move |_|leave(ui)>"Disconnect"</button>
    </aside></Show>}
}
fn debug_command(ui: Ui, command: &'static str) {
    leptos::task::spawn_local(async move {
        match native::<AudioStatus>(command, json!({})).await {
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
                on:click=move |_|leptos::task::spawn_local(async move {if let Err(error)=native::<()>("audio_debug_folder",json!({})).await{ui.status.set(error);}})>"Open recording folder"</button>
        </Show>
        <p class="text-xs text-white/60">"Join voice first. For a useful sample, have one person speak for 5–10 seconds, then talk at the same time. Delete the recording when it is no longer needed."</p>
    </section>}
}
#[component]
fn StreamVolumes(ui: Ui) -> impl IntoView {
    view! {<div class="space-y-3"><For each=move || { ui.audio_status.get().map(|s| s.streams).unwrap_or_default().into_iter().filter(|s| !s.target.is_some_and(|t| t.shared_audio)).collect::<Vec<_>>() } key=|s| (s.id.clone(), s.target) children=move |stream| view! { <StreamVolume ui=ui stream=stream/> }/></div>}
}

#[component]
pub(super) fn StreamVolume(ui: Ui, stream: StreamLevel) -> impl IntoView {
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
        if let Some(current) = current_volume.get() {
            gain.set((current * 100.0).round());
        }
    });
    let save = move |value: f32| {
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = native::<AudioStatus>(
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
        <div class="flex flex-wrap items-center gap-3">
            <label class="flex flex-wrap items-center gap-3">
                <span class="min-w-24 text-sm">{if shared_audio { "Screen share audio".to_owned() } else { stream.label }}</span>
                <input aria-label=if shared_audio { "Screen share volume" } else { "Speaker volume" } type="range" min="0" max="200" step="1"
                    disabled=move || busy.get() prop:value=move || gain.get().to_string()
                    on:input=move |e| { if let Ok(value) = event_target_value(&e).parse::<f32>() { gain.set(value); } }
                    on:change=move |e| { if let Ok(value) = event_target_value(&e).parse::<f32>() { save(value / 100.0); } }/>
                <span class="text-xs">{move || format!("{}%", gain.get())}</span>
            </label>
            {shared_audio.then(|| view! {
                <button class="rounded bg-white/10 px-3 py-2 text-sm disabled:opacity-50" disabled=move || busy.get()
                    aria-pressed=move || (gain.get() == 0.0).to_string()
                    on:click=move |_| {
                        if gain.get_untracked() > 0.0 {
                            restore.set(gain.get_untracked() / 100.0);
                            save(0.0);
                        } else { save(restore.get_untracked()); }
                    }>{move || if gain.get() == 0.0 { "Unmute screen share" } else { "Mute screen share" }}</button>
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
    let pending = RwSignal::new(false);
    view! {<section class="min-h-0 min-w-0 space-y-5 overflow-y-auto rounded-xl border border-white/10 p-5">
        <h2 class="text-xl font-semibold">{name}</h2>
        <Show when=desktop fallback=||view!{<p>"Open the desktop app to use native voice audio."</p>}>
            <p class="text-sm text-white/60" role="status">{move||ui.voice.get().message}</p>
            <Show when=move||ui.voice.get().channel_id!=Some(channel) fallback=move||view!{<button class="rounded bg-white/10 px-4 py-2" on:click=move |_|leave(ui)>"Leave voice"</button>}>
                <p class="text-sm text-white/60">"Joining uses your selected microphone. Use headphones for the best audio quality."</p>
                <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||pending.get() on:click=move |_|{
                    pending.set(true);let token=ui.token.get_untracked();let settings=ui.audio.get_untracked();
                    leptos::task::spawn_local(async move{let result=native::<()>("voice_join",json!({"token":token.unwrap_or_default(),"guildId":guild,"channelId":channel,"settings":settings})).await;if pending.try_get_untracked().is_some(){pending.set(false);}
                        if let Err(error)=result{ui.status.set(error);}});
                }>"Join voice"</button>
            </Show>
            <Show when=move||ui.voice.get().channel_id==Some(channel)>
                <ul class="space-y-2">{move||ui.voice.get().participants.into_iter().map(|m|view!{<li>{m.username}{if m.deafened{" · deafened"}else if m.muted||!m.can_speak{" · muted"}else{""}}</li>}).collect_view()}</ul>
                <super::screen::ScreenViewer ui=ui/>
                <h3 class="font-semibold">"Speaker volumes"</h3><StreamVolumes ui=ui/>
            </Show>
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
        if desktop() && ui.voice.get_untracked().channel_id.is_none() {
            leptos::task::spawn_local(async {
                let _ = native::<AudioStatus>("audio_stop", json!({})).await;
            });
        }
    });
    if desktop() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    loop {
                        match native::<Vec<AudioDevice>>("audio_devices", json!({})).await {
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
            let result = native::<AudioStatus>(
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
    view! {<div class="space-y-5"><h2 class="text-xl font-semibold">"Audio & voice"</h2>
        <Show when=desktop fallback=||view!{<p>"Audio devices and voice are available in the desktop app."</p>}>
            <p class="text-sm text-white/60">"Devices refresh automatically. Changes apply during a call; switching devices may briefly interrupt audio. Settings stay on this computer."</p>
            {[(true,"Microphone"),(false,"Speakers / headphones")].into_iter().map(move|(input,label)|view!{
                <label class="block space-y-2"><span>{label}</span><select class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" disabled=move||ui.audio_saving.get() prop:value=move||{let s=ui.audio.get();if input{s.input}else{s.output}.unwrap_or_default()} on:change=move|e|{let v=event_target_value(&e);ui.audio.update(|s|{let value=(!v.is_empty()).then_some(v);if input{s.input=value;}else{s.output=value;}});save(ui);}>
                    <option value="">"System default"</option>{move||devices.get().into_iter().filter(|d|d.input==input).map(|d|view!{<option value=d.id>{d.name}</option>}).collect_view()}
                </select></label>
            }).collect_view()}
            <label class="flex flex-wrap gap-3"><span>"Output volume"</span><input type="range" min="0" max="200" prop:value=move||(ui.audio.get().output_volume*100.0).to_string() on:change=move|e|{if let Ok(v)=event_target_value(&e).parse::<f32>(){ui.audio.update(|s|s.output_volume=v/100.0);save(ui);}}/></label>
            <label class="block space-y-2"><span>"Transmit mode"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move||if ui.audio.get().mode==TransmitMode::PushToTalk{"ptt"}else{"activity"} on:change=move|e|{ui.audio.update(|s|s.mode=if event_target_value(&e)=="ptt"{TransmitMode::PushToTalk}else{TransmitMode::VoiceActivity});save(ui);}><option value="activity">"Voice activation"</option><option value="ptt">"Push to talk"</option></select></label>
            <label class="flex flex-wrap gap-3"><span>"Activation threshold"</span><input type="range" min="1" max="100" prop:value=move||(ui.audio.get().activation_threshold*1000.0).to_string() on:change=move|e|{if let Ok(v)=event_target_value(&e).parse::<f32>(){ui.audio.update(|s|s.activation_threshold=v/1000.0);save(ui);}}/></label>
            <label class="flex items-center gap-3"><input type="checkbox" disabled=move||ui.audio_saving.get() prop:checked=move||ui.audio.get().global_push_to_talk on:change=move|e|{ui.audio.update(|s|s.global_push_to_talk=event_target_checked(&e));save(ui);}/>"Global push-to-talk: Ctrl+Shift+Space (Windows, macOS, Linux/X11)"</label>
            <div class="space-y-2">
                <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().noise_suppression on:change=move|e|{ui.audio.update(|s|s.noise_suppression=event_target_checked(&e));save(ui);}/>"Noise suppression"</label>
                <label class="block space-y-2"><span>"Noise suppression model"</span><select class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" disabled=move||ui.audio_saving.get() prop:value=move||match ui.audio.get().noise_suppression_model { NoiseSuppressionModel::Sonora=>"sonora", NoiseSuppressionModel::DeepFilterNet3=>"deep_filter_net3" } on:change=move|e|{ui.audio.update(|s|s.noise_suppression_model=if event_target_value(&e)=="deep_filter_net3" {NoiseSuppressionModel::DeepFilterNet3}else{NoiseSuppressionModel::Sonora});save(ui);}>
                    <option value="sonora">"Standard (Sonora)"</option><option value="deep_filter_net3">"DeepFilterNet3 (experimental)"</option>
                </select></label>
                <p class="text-sm text-white/60">"Models can be changed during a call. Echo cancellation needs time to adapt after a change. DeepFilterNet3 uses more CPU and adds buffering; test it with your microphone. It reduces noise but may preserve other people's voices."</p>
                <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().automatic_gain on:change=move|e|{ui.audio.update(|s|s.automatic_gain=event_target_checked(&e));save(ui);}/>"Automatic microphone gain"</label>
                <label class="flex gap-3"><input type="checkbox" prop:checked=move||ui.audio.get().echo_cancellation on:change=move|e|{ui.audio.update(|s|s.echo_cancellation=event_target_checked(&e));save(ui);}/>"Echo cancellation"</label>
                <label class="flex gap-3"><input type="checkbox" disabled=move||ui.audio_saving.get() prop:checked=move||ui.audio.get().neural_echo on:change=move|e|{ui.audio.update(|s|s.neural_echo=event_target_checked(&e));save(ui);}/>"Neural residual echo estimation (experimental)"</label>
                <Show when=move||ui.audio.get().neural_echo>
                    <details><summary class="cursor-pointer text-sm text-white/60">"Advanced model settings"</summary><label class="mt-2 block space-y-2"><span>"Model file override (optional)"</span><input type="text" class="w-full rounded bg-slate-900 p-2 disabled:opacity-50" placeholder="Bundled REE v2 model" disabled=move||ui.audio_saving.get() prop:value=move||ui.audio.get().neural_echo_model.unwrap_or_default() on:change=move|e|{let value=event_target_value(&e);ui.audio.update(|s|s.neural_echo_model=(!value.trim().is_empty()).then_some(value));save(ui);}/></label></details>
                    <p class="text-sm text-slate-400">"Uses the bundled model when the override is empty. Enable echo cancellation too. Changes apply without leaving voice."</p>
                </Show>
            </div>
            <Show when=move||ui.audio_saving.get()><p class="text-sm text-white/60" role="status">"Applying audio settings..."</p></Show>
            <VoiceControls ui=ui/>
            <DebugRecording ui=ui/>
            <label class="block">"Raw microphone"<meter class="ml-3 w-48" min="0" max="1" value=move||ui.audio_status.get().map(|s|s.raw_input_level).unwrap_or(0.0) /></label>
            <label class="block">"After noise / echo processing"<meter class="ml-3 w-48" min="0" max="1" value=move||ui.audio_status.get().map(|s|s.input_level).unwrap_or(0.0) /></label>
            <p class="text-sm" role="status">{move||ui.audio_status.get().map(|s|format!("{} · dropped {} · underruns {} · processing resets {}",s.message,s.dropped_samples,s.underrun_samples,s.processing_resets))}</p>
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
            <div class="flex flex-wrap gap-3">
                <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||busy.get()||ui.voice.get().channel_id.is_some() on:click=move |_|test(false)>"Test playback (5 seconds)"</button>
                <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-50" disabled=move||busy.get()||ui.voice.get().channel_id.is_some() on:click=move |_|test(true)>"Test microphone (60 seconds)"</button>
                <button class="underline" on:click=move |_|{leave(ui);leptos::task::spawn_local(async{let _=native::<AudioStatus>("audio_stop",json!({})).await;});}>"Stop audio"</button>
                <button class="underline disabled:opacity-50" disabled=move||busy.get() on:click=move |_|{busy.set(true);message.set("Testing encrypted WebRTC forwarding…".into());leptos::task::spawn_local(async move{let result=native::<String>("audio_webrtc_probe",json!({})).await;if busy.try_get_untracked().is_some(){busy.set(false);message.set(result.unwrap_or_else(|e|e));}});}>"Test WebRTC"</button>
            </div>
            <StreamVolumes ui=ui/><p role="status" class="text-sm text-white/70">{move||message.get()}</p>
        </Show>
    </div>}
}
