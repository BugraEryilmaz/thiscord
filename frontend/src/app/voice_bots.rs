use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{ChannelId, GuildId, voice_bots::*};

#[derive(Clone)]
pub(super) struct Target {
    pub guild: GuildId,
    pub channel: ChannelId,
    pub name: String,
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    Effect::new(move |_| {
        let _ = ui.token.get();
        ui.bot_target.set(None);
    });
    view! {<For each=move ||{ui.bot_target.get().into_iter().collect::<Vec<_>>()} key=|target|(target.guild,target.channel) children=move |target|view!{<Panel ui=ui target=target/>}/>}
}

#[component]
fn Panel(ui: Ui, target: Target) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let bots = RwSignal::new(Vec::<Bot>::new());
    let busy = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let draft = RwSignal::new(Settings::default());
    let command = Callback::new(move |command: Command| {
        if busy.get_untracked() {
            return;
        }
        let adding = matches!(command, Command::Add { .. });
        busy.set(true);
        error.set(String::new());
        let token = ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let request = Request {
                guild_id: target.guild,
                channel_id: target.channel,
                command,
            };
            let result =
                crate::account_client::api_request::<_, Response>(PATH, &request, token.as_deref())
                    .await;
            if bots.is_disposed() || ui.token.try_get_untracked().flatten() != token {
                return;
            }
            busy.set(false);
            match result {
                Ok(response) => {
                    if adding {
                        draft.update(|s| {
                            s.microphone_loop =
                                MicrophoneLoop::ALL[response.bots.len() % MicrophoneLoop::ALL.len()]
                        });
                    }
                    bots.set(response.bots);
                    loaded.set(true);
                }
                Err(message) => {
                    error.set(thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "{message}. Refresh before retrying.",
                        message = crate::i18n::message(message)
                    ));
                    loaded.set(false);
                }
            }
        });
    });
    command.run(Command::List {});
    Effect::new(move |_| {
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    });
    view! {
        <dialog node_ref=dialog on:cancel=move |_:web_sys::Event|ui.bot_target.set(None) class="fixed inset-0 m-auto max-h-[90vh] w-full max-w-3xl overflow-auto rounded-xl border border-slate-700 bg-slate-950 p-6 text-slate-100 backdrop:bg-black/70">
            <header class="mb-4 flex items-center justify-between gap-4"><h2 class="text-xl font-semibold">{move || thiscord_frontend::localized_format!(crate::i18n::language(), "Voice test bots · {}",target.name)}</h2><button autofocus class="rounded bg-slate-800 px-4 py-2" on:click=move |_|ui.bot_target.set(None)>{move || crate::i18n::t("Close")}</button></header>
            <p class="mb-4 text-sm text-slate-400">{move || crate::i18n::t("Bots join as separate voice participants and play a repeating microphone loop. They use channel slots and leave after one hour, when your session ends, or when access is revoked.")}</p>
            <p role="status" class="my-3 text-amber-300">{move ||crate::i18n::message(error.get())}</p>
            <div class="mb-4 flex items-center gap-3"><button class="rounded bg-slate-800 px-4 py-2 disabled:opacity-50" disabled=move ||busy.get() on:click=move |_|command.run(Command::List {})>{move || crate::i18n::t("Refresh")}</button><span role="status" class="text-sm text-slate-400">{move ||if busy.get(){crate::i18n::t("Working…").into()}else{thiscord_frontend::localized_format!(crate::i18n::language(), "{} test bots",bots.get().len())}}</span></div>
            <fieldset disabled=move ||busy.get() || !loaded.get() class="space-y-4 disabled:opacity-50">
                <section class="rounded-lg border border-slate-700 p-4"><h3 class="mb-3 font-semibold">{move || crate::i18n::t("Add a test bot")}</h3><Options settings=draft/>
                    <button
                        class="mt-4 rounded bg-indigo-600 px-4 py-2 disabled:opacity-50"
                        disabled={move || bots.get().len() >= thiscord_shared::voice::ROOM_CAPACITY}
                        on:click={move |_| command.run(Command::Add { settings: draft.get_untracked() })}
                    >{move || crate::i18n::t("Add bot to this channel")}</button>
                </section>
                <For each=move ||bots.get() key=|bot|bot.id children=move |bot|view!{<BotEditor ui=ui channel=target.channel bot=bot bots=bots command=command/>}/>
            </fieldset>
            <p class="mt-5 text-xs text-slate-400">{move || crate::i18n::t("Connection profiles simulate media bandwidth, delay, jitter and packet loss in both directions. Bots automatically watch cameras and screen shares, including shared audio. Refresh to see decoded video frames. They decode media without speakers or a display.")}</p>
        </dialog>
    }
}

#[component]
fn Options(settings: RwSignal<Settings>) -> impl IntoView {
    view! {<div class="grid gap-4 sm:grid-cols-2">
        <label class="text-sm">{move || crate::i18n::t("Connection profile")}<select class="mt-1 block w-full rounded bg-slate-800 p-2" prop:value=move ||ConnectionProfile::ALL.iter().position(|p|*p==settings.get().profile).unwrap_or(0).to_string() on:change=move |event|{if let Ok(index)=event_target_value(&event).parse::<usize>() && let Some(profile)=ConnectionProfile::ALL.get(index){settings.update(|s|s.profile = *profile);}}>
            {ConnectionProfile::ALL.into_iter().enumerate().map(|(i,p)|view!{<option value=i.to_string()>{move ||crate::i18n::t(p.label())}</option>}).collect_view()}
        </select><span class="mt-1 block text-xs text-slate-400">{move ||{let (delay,jitter,loss,bits)=settings.get().profile.parameters();thiscord_frontend::localized_format!(crate::i18n::language(), "Audio {} kbit/s · video {} kbit/s · {}–{} ms one way · {}% loss",bits/1000,settings.get().profile.video_bits()/1000,delay,delay+jitter,loss)}}</span></label>
        <label class="text-sm">{move || crate::i18n::t("Microphone loop")}<select class="mt-1 block w-full rounded bg-slate-800 p-2" prop:value=move ||MicrophoneLoop::ALL.iter().position(|p|*p==settings.get().microphone_loop).unwrap_or(0).to_string() on:change=move |event|{if let Ok(index)=event_target_value(&event).parse::<usize>() && let Some(value)=MicrophoneLoop::ALL.get(index){settings.update(|s|s.microphone_loop = *value);}}>
            {MicrophoneLoop::ALL.into_iter().enumerate().map(|(i,p)|view!{<option value=i.to_string()>{move ||crate::i18n::t(p.label())}</option>}).collect_view()}
        </select></label>
        <label class="text-sm">{move || crate::i18n::t("Microphone scenario")}<select class="mt-1 block w-full rounded bg-slate-800 p-2" prop:value=move ||Scenario::ALL.iter().position(|v|*v==settings.get().scenario).unwrap_or(0).to_string() on:change=move |event|{if let Ok(index)=event_target_value(&event).parse::<usize>() && let Some(value)=Scenario::ALL.get(index){settings.update(|s|s.scenario = *value);}}>
            {Scenario::ALL.into_iter().enumerate().map(|(i,s)|view!{<option value=i.to_string()>{move || crate::i18n::t(s.label())}</option>}).collect_view()}
        </select></label>
    </div>}
}
#[component]
fn BotEditor(
    ui: Ui,
    channel: ChannelId,
    bot: Bot,
    bots: RwSignal<Vec<Bot>>,
    command: Callback<Command>,
) -> impl IntoView {
    let settings = RwSignal::new(bot.settings.clone());
    let original = StoredValue::new(bot.settings.clone());
    let last_revision = StoredValue::new(bot.revision);
    Effect::new(move |_| {
        bots.with(|entries| {
            if let Some(current) = entries.iter().find(|b| b.id == bot.id)
                && current.revision != last_revision.get_value()
            {
                last_revision.set_value(current.revision);
                original.set_value(current.settings.clone());
                settings.set(current.settings.clone());
            }
        });
    });
    let metrics = move || {
        bots.with(|entries| entries.iter().find(|b| b.id == bot.id)
        .map(|b| thiscord_frontend::localized_format!(crate::i18n::language(), "Sent {} · uplink dropped {} · received {} · output peak {:.0}% (at last refresh)", b.sent_packets, b.dropped_packets, b.received_packets, b.output_peak * 100.0))
        .unwrap_or_default())
    };
    let video_metrics = move || {
        bots.with(|entries| entries.iter().find(|b| b.id == bot.id).map(|b| {
        let mut lines = vec![thiscord_frontend::localized_format!(crate::i18n::language(), "Shared audio: {} received packets", b.received_system_audio_packets)];
        lines.extend(b.video.iter().map(|v| thiscord_frontend::localized_format!(crate::i18n::language(), "{} · slot {} · {}×{} · {} decoded frames · {} dropped / {} missing packets · {} decode errors · {} keyframe requests · {} kbit/s", if v.kind == thiscord_shared::voice::MediaKind::Camera { crate::i18n::t("Camera") } else { crate::i18n::t("Screen share") }, v.slot + 1, v.width, v.height, v.decoded_frames, v.dropped_packets, v.missing_packets, v.decode_errors, v.keyframe_requests, v.bitrate / 1000)));
        lines.join("\n")
    }).unwrap_or_default())
    };
    view! {<section class="rounded-lg border border-slate-700 p-4">
        <h3 class="mb-3 font-semibold">{bot.name}</h3><Options settings=settings/>
        <label class="my-4 flex gap-3 text-sm"><input type="checkbox" prop:checked=move ||settings.get().talking on:change=move |event|settings.update(|s|s.talking=event_target_checked(&event))/>{move || crate::i18n::t("Hold virtual push-to-talk key (apply test conditions)")}</label>
        <BotAudio ui=ui channel=channel connection=bot.connection_id/>
        <p class="mb-2 text-xs text-slate-400">{metrics}</p>
        <p class="mb-4 whitespace-pre-line text-xs text-slate-400">{video_metrics}</p>
        <div class="flex gap-3"><button class="rounded bg-indigo-600 px-4 py-2 disabled:opacity-50" disabled=move ||settings.get()==original.get_value() on:click=move |_|command.run(Command::Update {bot_id:bot.id,revision:last_revision.get_value(),settings:settings.get_untracked()})>{move || crate::i18n::t("Apply test conditions")}</button><button class="rounded bg-slate-800 px-4 py-2" on:click=move |_|settings.set(original.get_value())>{move || crate::i18n::t("Reset")}</button><button class="ml-auto rounded bg-red-500/20 px-4 py-2 text-red-200" on:click=move |_|command.run(Command::Remove {bot_id:bot.id})>{move || crate::i18n::t("Remove bot")}</button></div>
    </section>}
}

#[component]
fn BotAudio(
    ui: Ui,
    channel: ChannelId,
    connection: Option<thiscord_shared::RequestId>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <button class="my-3 rounded bg-indigo-600 px-4 py-2" on:click=move |_|open.update(|v| *v = !*v)>{move ||if open.get() { crate::i18n::t("Close voice settings") } else { crate::i18n::t("Voice settings and diagnostics") }}</button>
        <Show when=move ||open.get()>{move ||connection.map(|connection|view!{<BotAudioSettings ui=ui channel=channel connection=connection/>})}</Show>
    }
}
#[component]
fn BotAudioSettings(
    ui: Ui,
    channel: ChannelId,
    connection: thiscord_shared::RequestId,
) -> impl IntoView {
    let ui = Ui {
        audio_control: crate::audio_control::AudioControl::remote(connection, ui.token).for_bot(),
        audio: RwSignal::new(Default::default()),
        audio_status: RwSignal::new(None),
        audio_saving: RwSignal::new(false),
        audio_revision: RwSignal::new(0),
        voice: RwSignal::new(thiscord_shared::voice::VoiceStatus {
            channel_id: Some(channel),
            ..Default::default()
        }),
        status: RwSignal::new(String::new()),
        ..ui
    };
    let loaded = RwSignal::new(false);
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    leptos::task::spawn_local(async move {
        let _ = futures_util::future::Abortable::new(
            async move {
                loop {
                    if !ui.audio_saving.get_untracked() {
                        let revision = ui.audio_revision.get_untracked();
                        let settings = ui
                            .audio_control
                            .invoke("audio_current", serde_json::json!({}))
                            .await;
                        let status = ui
                            .audio_control
                            .invoke("audio_status", serde_json::json!({}))
                            .await;
                        match (settings, status) {
                            (Ok(settings), Ok(status)) => {
                                if !ui.audio_saving.get_untracked()
                                    && ui.audio_revision.get_untracked() == revision
                                {
                                    ui.audio_control.observe(&settings);
                                    ui.audio.set(settings);
                                }
                                ui.audio_status.set(Some(status));
                                loaded.set(true);
                            }
                            (Err(error), _) | (_, Err(error)) => {
                                ui.status.set(error);
                                loaded.set(false);
                            }
                        }
                    }
                    gloo_timers::future::TimeoutFuture::new(3000).await;
                }
            },
            registration,
        )
        .await;
    });
    view! {
        <p class="mb-4 text-sm text-slate-400">{move ||crate::i18n::t("Uses the desktop voice engine and settings. Audio changes save automatically. Test conditions apply separately. Virtual devices replace physical hardware; keyboard shortcuts and overlays are unavailable. Speaker echo requires another participant sending audio. DRED recovery requires DRED on both participants.")}</p>
        <p role="status" class="text-amber-300">{move ||ui.status.get()}</p>
        <fieldset disabled=move ||!loaded.get() || ui.audio_saving.get() class="space-y-4 disabled:opacity-50">
            <div class="flex flex-wrap gap-2">{[("Opus baseline",false,false),("RED",true,false),("DRED",false,true),("RED + DRED",true,true)].into_iter().map(move |(label,red,dred)|view!{
                <button class="rounded bg-slate-800 px-3 py-2 text-sm" on:click=move |_|{ui.audio.update(|s|{s.packet_redundancy=red;s.deep_redundancy=dred;});super::audio::save(ui);}>{move ||crate::i18n::t(label)}</button>
            }).collect_view()}</div>
            <super::audio::AudioSettingsPanel ui=ui/>
        </fieldset>
    }
}
