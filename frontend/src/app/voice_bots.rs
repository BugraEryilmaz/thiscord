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
                <For each=move ||bots.get() key=|bot|(bot.id,bot.revision) children=move |bot|view!{<BotEditor bot=bot bots=bots command=command/>}/>
            </fieldset>
            <p class="mt-5 text-xs text-slate-400">{move || crate::i18n::t("Connection profiles simulate media bandwidth, delay, jitter and packet loss in both directions. Bots receive and decode voice into a silent output. They have no physical devices, camera or desktop audio processing.")}</p>
        </dialog>
    }
}

#[component]
fn Options(settings: RwSignal<Settings>) -> impl IntoView {
    view! {<div class="grid gap-4 sm:grid-cols-2">
        <label class="text-sm">{move || crate::i18n::t("Connection profile")}<select class="mt-1 block w-full rounded bg-slate-800 p-2" prop:value=move ||ConnectionProfile::ALL.iter().position(|p|*p==settings.get().profile).unwrap_or(0).to_string() on:change=move |event|{if let Ok(index)=event_target_value(&event).parse::<usize>() && let Some(profile)=ConnectionProfile::ALL.get(index){settings.update(|s|s.profile = *profile);}}>
            {ConnectionProfile::ALL.into_iter().enumerate().map(|(i,p)|view!{<option value=i.to_string()>{move ||crate::i18n::t(p.label())}</option>}).collect_view()}
        </select><span class="mt-1 block text-xs text-slate-400">{move ||{let (delay,jitter,loss,bits)=settings.get().profile.parameters();thiscord_frontend::localized_format!(crate::i18n::language(), "{} kbit/s · {}–{} ms one way · {}% loss",bits/1000,delay,delay+jitter,loss)}}</span></label>
        <label class="text-sm">{move || crate::i18n::t("Microphone loop")}<select class="mt-1 block w-full rounded bg-slate-800 p-2" prop:value=move ||MicrophoneLoop::ALL.iter().position(|p|*p==settings.get().microphone_loop).unwrap_or(0).to_string() on:change=move |event|{if let Ok(index)=event_target_value(&event).parse::<usize>() && let Some(value)=MicrophoneLoop::ALL.get(index){settings.update(|s|s.microphone_loop = *value);}}>
            {MicrophoneLoop::ALL.into_iter().enumerate().map(|(i,p)|view!{<option value=i.to_string()>{move ||crate::i18n::t(p.label())}</option>}).collect_view()}
        </select></label>
    </div>}
}
#[component]
fn BotEditor(bot: Bot, bots: RwSignal<Vec<Bot>>, command: Callback<Command>) -> impl IntoView {
    let settings = RwSignal::new(bot.settings.clone());
    let original = StoredValue::new(bot.settings.clone());
    let metrics = move || {
        bots.with(|entries| entries.iter().find(|b| b.id == bot.id)
        .map(|b| thiscord_frontend::localized_format!(crate::i18n::language(), "Sent {} · uplink dropped {} · decoded {} · output peak {:.0}% (at last refresh)", b.sent_packets, b.dropped_packets, b.received_packets, b.output_peak * 100.0))
        .unwrap_or_default())
    };
    view! {<section class="rounded-lg border border-slate-700 p-4">
        <h3 class="mb-3 font-semibold">{bot.name}</h3><Options settings=settings/>
        <div class="my-4 grid gap-4 sm:grid-cols-2">
            <label class="text-sm">{move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Microphone gain · {:.1}×",settings.get().input_gain)}<input class="mt-2 block w-full" type="range" min="1" max="10" step="0.1" prop:value=move ||settings.get().input_gain on:input=move |event|if let Ok(value)=event_target_value(&event).parse(){settings.update(|s|s.input_gain=value);}/></label>
            <label class="text-sm">{move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Output volume · {:.0}%",settings.get().output_volume*100.0)}<input class="mt-2 block w-full" type="range" min="0" max="2" step="0.01" prop:value=move ||settings.get().output_volume on:input=move |event|if let Ok(value)=event_target_value(&event).parse(){settings.update(|s|s.output_volume=value);}/></label>
        </div>
        <div class="my-4 flex gap-6 text-sm"><label><input type="checkbox" prop:checked=move ||settings.get().muted on:change=move |event|settings.update(|s|s.muted=event_target_checked(&event))/>{move || crate::i18n::t(" Mute microphone")}</label><label><input type="checkbox" prop:checked=move ||settings.get().deafened on:change=move |event|settings.update(|s|s.deafened=event_target_checked(&event))/>{move || crate::i18n::t(" Deafen")}</label></div>
        <p class="mb-4 text-xs text-slate-400">{metrics}</p>
        <div class="flex gap-3"><button class="rounded bg-indigo-600 px-4 py-2 disabled:opacity-50" disabled=move ||settings.get()==original.get_value() on:click=move |_|command.run(Command::Update {bot_id:bot.id,revision:bot.revision,settings:settings.get_untracked()})>{move || crate::i18n::t("Save audio settings")}</button><button class="rounded bg-slate-800 px-4 py-2" on:click=move |_|settings.set(original.get_value())>{move || crate::i18n::t("Reset")}</button><button class="ml-auto rounded bg-red-500/20 px-4 py-2 text-red-200" on:click=move |_|command.run(Command::Remove {bot_id:bot.id})>{move || crate::i18n::t("Remove bot")}</button></div>
    </section>}
}
