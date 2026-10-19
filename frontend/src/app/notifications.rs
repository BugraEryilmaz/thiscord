use super::Ui;
use crate::account_client as client;
use futures_util::{
    SinkExt, StreamExt,
    future::{AbortHandle, Abortable, Either, select},
};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;
use thiscord_shared::notifications::*;
use wasm_bindgen::JsCast;

#[derive(Clone, Copy)]
pub(super) struct State {
    pub preferences: RwSignal<Preferences>,
    connected: RwSignal<bool>,
    saving: RwSignal<bool>,
    queue: RwSignal<thiscord_frontend::notification_queue::PreferenceQueue>,
    generation: RwSignal<Option<u32>>,
    ringing: RwSignal<Vec<thiscord_shared::ids::CallId>>,
    pub editor: RwSignal<Option<(Target, String)>>,
    banners: RwSignal<Vec<Alert>>,
    error: RwSignal<String>,
    now: RwSignal<f64>,
}
impl State {
    pub fn new() -> Self {
        Self {
            preferences: RwSignal::new(Preferences::default()),
            connected: RwSignal::new(false),
            saving: RwSignal::new(false),
            queue: RwSignal::new(Default::default()),
            generation: RwSignal::new(None),
            ringing: RwSignal::new(vec![]),
            editor: RwSignal::new(None),
            banners: RwSignal::new(vec![]),
            error: RwSignal::new(String::new()),
            now: RwSignal::new(js_sys::Date::now()),
        }
    }
}
fn apply(ui: Ui, prefs: Preferences) {
    if prefs.revision >= ui.notifications.preferences.get_untracked().revision {
        ui.notifications.preferences.set(prefs);
    }
}
pub(super) fn request(ui: Ui, command: Request) {
    if let Some(Err(error)) = ui.notifications.queue.try_update(|q| q.push(command)) {
        ui.notifications.error.set(error.into());
        ui.status.set(error.into());
        return;
    }
    if ui.notifications.saving.get_untracked() {
        return;
    }
    ui.notifications.saving.set(true);
    ui.notifications.error.set(String::new());
    let token = ui.token.get_untracked();
    leptos::task::spawn_local(async move {
        loop {
            if token != ui.token.get_untracked() {
                return;
            }
            let command = ui
                .notifications
                .queue
                .try_update(|q| q.next(ui.notifications.preferences.get_untracked().revision))
                .flatten();
            let Some(command) = command else {
                ui.notifications.saving.set(false);
                break;
            };
            let result =
                client::api_request::<_, Preferences>(PATH, &command, token.as_deref()).await;
            if token != ui.token.get_untracked() {
                return;
            }
            match result {
                Ok(prefs) => {
                    apply(ui, prefs);
                    if let Request::Snooze { target, .. } = command {
                        ui.notifications
                            .banners
                            .update(|items| items.retain(|a| a.target != target));
                        if client::desktop() {
                            let _ = client::native::<()>(
                                "notification_snoozed",
                                serde_json::json!({"target":target,"generation":ui.notifications.generation.get_untracked()}),
                            )
                            .await;
                            if token != ui.token.get_untracked() {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    ui.notifications.error.set(error.clone());
                    ui.status.set(error);
                    if let Ok(prefs) =
                        client::api_request(PATH, &Request::Get, token.as_deref()).await
                        && token == ui.token.get_untracked()
                    {
                        apply(ui, prefs);
                    }
                }
            }
        }
    });
}
pub(super) fn snooze(ui: Ui, target: Target, duration: Snooze) {
    request(ui, Request::Snooze { target, duration });
}
pub(super) fn edit(ui: Ui, target: Target, title: String) {
    ui.notifications.editor.set(Some((target, title)));
}
pub(super) fn muted(ui: Ui, target: Target) -> bool {
    ui.notifications
        .preferences
        .get()
        .subscriptions
        .iter()
        .any(|s| {
            s.target == target
                && match s.mute {
                    Mute::None => false,
                    Mute::Forever => true,
                    Mute::Until { at } => at.timestamp_millis() as f64 > ui.notifications.now.get(),
                }
        })
}
fn sound(ui: Ui, kind: Kind, volume: u8) {
    if ui.audio.get_untracked().deafened || volume == 0 {
        return;
    }
    if client::desktop() {
        leptos::task::spawn_local(async move {
            let _ = client::native::<()>(
                "notification_sound",
                serde_json::json!({"kind":kind,"volume":volume}),
            )
            .await;
        });
    } else {
        // Browser autoplay rules apply. Creating the context in a preview button
        // gesture enables sound without persisting any account data in the browser.
        BROWSER_AUDIO.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = web_sys::AudioContext::new()
                    .ok()
                    .map(|context| BrowserAudio {
                        context,
                        active: None,
                    });
            }
            if let Some(audio) = slot.as_mut() {
                let context = &audio.context;
                let _ = context.resume();
                let samples = thiscord_frontend::notification_sound::render(
                    kind,
                    context.sample_rate() as u32,
                );
                if let (Ok(buffer), Ok(source), Ok(gain)) = (
                    context.create_buffer(1, samples.len() as u32, context.sample_rate()),
                    context.create_buffer_source(),
                    context.create_gain(),
                ) {
                    if buffer.copy_to_channel(&samples, 0).is_err() {
                        return;
                    }
                    source.set_buffer(Some(&buffer));
                    gain.gain().set_value(f32::from(volume.min(100)) / 100.0);
                    if source.connect_with_audio_node(&gain).is_err()
                        || gain
                            .connect_with_audio_node(&context.destination())
                            .is_err()
                    {
                        return;
                    }
                    // Bound browser playback to one sound; rapid previews must
                    // not layer into an indistinguishable, louder chord.
                    if let Some(previous) = audio.active.take() {
                        let _ = previous
                            .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
                            .stop();
                        let _ = previous.disconnect();
                    }
                    if source.start().is_err() {
                        let _ = source.disconnect();
                        let _ = gain.disconnect();
                        return;
                    }
                    let cleanup = source.clone();
                    let ended = wasm_bindgen::closure::Closure::once_into_js(move || {
                        let _ = cleanup.disconnect();
                        let _ = gain.disconnect();
                        cleanup
                            .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
                            .set_onended(None);
                    });
                    source
                        .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
                        .set_onended(Some(ended.unchecked_ref()));
                    audio.active = Some(source);
                }
            }
        });
    }
}
struct BrowserAudio {
    context: web_sys::AudioContext,
    active: Option<web_sys::AudioBufferSourceNode>,
}
thread_local! { static BROWSER_AUDIO: std::cell::RefCell<Option<BrowserAudio>> = const { std::cell::RefCell::new(None) }; }
fn receive(ui: Ui, mut alert: Alert, replayed: bool) {
    if let Target::Conversation { conversation_id } = alert.target
        && let Some(friend) = ui
            .friends
            .snapshot
            .get_untracked()
            .friends
            .iter()
            .find(|f| f.conversation_id == conversation_id)
    {
        alert.title = format!("{} — {}", friend.person.display_name, alert.title);
    }
    if alert.kind == Kind::Update {
        let Some(version) = &alert.version else {
            return;
        };
        let parts = |v: &str| {
            v.split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        };
        if parts(version) <= parts(env!("CARGO_PKG_VERSION")) {
            return;
        }
        if client::desktop() {
            leptos::task::spawn_local(async {
                let _ = client::native::<()>("update_check", serde_json::json!({})).await;
            });
        }
    }
    if alert.delivery.sound && !replayed {
        sound(ui, alert.kind, alert.delivery.volume);
    }
    if !alert.delivery.banner {
        return;
    }
    if client::desktop() {
        let token = ui.token.get_untracked();
        let generation = ui.notifications.generation.get_untracked();
        leptos::task::spawn_local(async move {
            if token != ui.token.get_untracked() {
                return;
            }
            if let Err(error) = client::native::<()>(
                "notification_show",
                serde_json::json!({"alert":alert,"generation":generation}),
            )
            .await
                && token == ui.token.get_untracked()
                && generation == ui.notifications.generation.get_untracked()
            {
                ui.notifications.error.set(error);
                show_in_app(ui, alert);
            }
        });
    } else {
        show_in_app(ui, alert);
    }
}
fn show_in_app(ui: Ui, alert: Alert) {
    if !thiscord_frontend::notification_queue::can_present(
        &alert,
        &ui.notifications.ringing.get_untracked(),
    ) {
        return;
    }
    let id = alert.id.clone();
    let timeout = if alert.kind == Kind::IncomingCall {
        30000
    } else {
        12000
    };
    ui.notifications.banners.update(|items| {
        items.retain(|a| a.id != id);
        items.push(alert);
        if items.len() > 3 {
            items.remove(0);
        }
    });
    leptos::task::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(timeout).await;
        if !ui.notifications.banners.is_disposed() {
            ui.notifications
                .banners
                .update(|items| items.retain(|a| a.id != id));
        }
    });
}
fn interaction(ui: Ui, alert: Alert, action: BannerAction) {
    if let BannerAction::Snooze { duration } = action {
        snooze(ui, alert.target, duration);
        return;
    }
    if matches!(action, BannerAction::Accept) && !client::desktop() && !ui.audio_control.is_remote()
    {
        ui.status
            .set("Open the desktop app to accept this call.".into());
        return;
    }
    ui.notifications
        .banners
        .update(|items| items.retain(|a| a.id != alert.id));
    match action {
        BannerAction::Snooze { duration } => snooze(ui, alert.target, duration),
        BannerAction::Accept | BannerAction::Decline => {
            if let Some(call_id) = alert.call_id {
                super::friends::action(
                    ui,
                    if matches!(action, BannerAction::Accept) {
                        thiscord_shared::friends::Request::AcceptCall { call_id }
                    } else {
                        thiscord_shared::friends::Request::EndCall { call_id }
                    },
                );
            }
        }
        BannerAction::Open => match alert.target {
            Target::Conversation { conversation_id } => super::friends::open(ui, conversation_id),
            Target::Channel {
                guild_id,
                channel_id,
            } => leptos::task::spawn_local(async move {
                if let Err(error) = super::servers::select(ui, guild_id).await {
                    ui.status.set(error);
                    return;
                }
                ui.selected_channel.set(
                    ui.server
                        .get_untracked()
                        .and_then(|s| s.channels.into_iter().find(|c| c.id == channel_id)),
                );
                ui.page.set("servers");
            }),
            Target::Updates => super::settings::navigate(ui, "updates"),
        },
        BannerAction::Dismiss => {}
    }
}
#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let expiry = StoredValue::new(None::<AbortHandle>);
    Effect::new(move |_| {
        let prefs = ui.notifications.preferences.get();
        let now = ui.notifications.now.get().max(js_sys::Date::now());
        if let Some(handle) = expiry.get_value() {
            handle.abort();
        }
        let next = prefs
            .subscriptions
            .iter()
            .filter_map(|s| match s.mute {
                Mute::Until { at } => Some(at.timestamp_millis() as f64),
                _ => None,
            })
            .filter(|at| *at > now)
            .min_by(f64::total_cmp);
        if let Some(next) = next {
            let (handle, registration) = AbortHandle::new_pair();
            expiry.set_value(Some(handle));
            leptos::task::spawn_local(async move {
                let _ = Abortable::new(
                    async move {
                        gloo_timers::future::TimeoutFuture::new(
                            ((next - now).ceil() as u32).clamp(1, 2_147_000_000),
                        )
                        .await;
                        ui.notifications.now.set(js_sys::Date::now());
                    },
                    registration,
                )
                .await;
            });
        }
    });
    on_cleanup(move || {
        if let Some(handle) = expiry.get_value() {
            handle.abort();
        }
    });
    let abort = StoredValue::new(None::<AbortHandle>);
    let desktop_listener = StoredValue::new_local(None::<client::EventListener>);
    if client::desktop() {
        leptos::task::spawn_local(async move {
            if let Ok(listener) =
                client::listen("notification-action", move |event: BannerInteraction| {
                    if ui.token.get_untracked().is_some()
                        && ui.notifications.generation.get_untracked() == Some(event.generation)
                    {
                        interaction(ui, event.alert, event.action);
                    }
                })
                .await
                && !desktop_listener.is_disposed()
            {
                desktop_listener.set_value(Some(listener));
            }
        });
    }
    on_cleanup(move || {
        if let Some(abort) = abort.get_value() {
            abort.abort();
        }
        desktop_listener.set_value(None);
    });
    Effect::new(move |_| {
        let token = ui.token.get();
        if let Some(abort) = abort.get_value() {
            abort.abort();
        }
        ui.notifications.preferences.set(Preferences::default());
        ui.notifications.connected.set(false);
        ui.notifications.banners.set(vec![]);
        ui.notifications.saving.set(false);
        ui.notifications.queue.set(Default::default());
        ui.notifications.generation.set(None);
        ui.notifications.ringing.set(vec![]);
        ui.notifications.editor.set(None);
        let Some(token) = token else {
            if client::desktop() {
                leptos::task::spawn_local(async {
                    let _ =
                        client::native::<u32>("notification_clear", serde_json::json!({})).await;
                });
            }
            return;
        };
        let (handle, registration) = AbortHandle::new_pair();
        abort.set_value(Some(handle));
        leptos::task::spawn_local(async move {
            let _ = Abortable::new(
                async move {
                    if client::desktop()
                        && let Ok(generation) =
                            client::native::<u32>("notification_clear", serde_json::json!({})).await
                    {
                        ui.notifications.generation.set(Some(generation));
                    }
                    let mut seen = std::collections::VecDeque::<String>::new();
                    let mut delay = 1000;
                    loop {
                        let start = js_sys::Date::now();
                        let _ = connected(ui, &token, &mut seen).await;
                        ui.notifications.connected.set(false);
                        sync_calls(ui, vec![]).await;
                        if js_sys::Date::now() - start > 10000.0 {
                            delay = 1000;
                        }
                        gloo_timers::future::TimeoutFuture::new(
                            delay + (js_sys::Math::random() * 500.0) as u32,
                        )
                        .await;
                        delay = (delay * 2).min(30000);
                    }
                },
                registration,
            )
            .await;
        });
    });
    view! {
        <aside class="fixed bottom-6 right-6 z-50 w-[380px] max-w-[90vw] space-y-2" aria-live="polite"><For each=move ||ui.notifications.banners.get() key=|a|a.id.clone() children=move |alert| {
            let saved=StoredValue::new(alert.clone()); view!{<crate::notification_banner::Banner alert=alert can_accept=client::desktop()||ui.audio_control.is_remote() action=Callback::new(move |action|interaction(ui,saved.get_value(),action))/>}
        }/></aside>
        <Editor ui=ui/>
    }
}
async fn sync_calls(ui: Ui, ringing: Vec<thiscord_shared::ids::CallId>) {
    ui.notifications.ringing.set(ringing.clone());
    ui.notifications
        .banners
        .update(|items| items.retain(|a| a.call_id.is_none_or(|id| ringing.contains(&id))));
    if client::desktop() {
        let _ = client::native::<()>(
            "notification_sync_calls",
            serde_json::json!({"ringing":ringing,"generation":ui.notifications.generation.get_untracked()}),
        )
        .await;
    }
}
async fn connected(
    ui: Ui,
    token: &str,
    seen: &mut std::collections::VecDeque<String>,
) -> Result<(), ()> {
    let base = client::api_base()
        .trim_end_matches('/')
        .replacen("http", "ws", 1);
    let (mut sink, mut stream) = WebSocket::open(&format!("{base}{SOCKET_PATH}"))
        .map_err(|_| ())?
        .split();
    let encode = |event| {
        serde_json::to_string(&ClientFrame {
            version: VERSION,
            event,
        })
        .map(Message::Text)
        .map_err(|_| ())
    };
    sink.send(encode(ClientEvent::Connect {
        token: token.into(),
        kinds: Kind::ALL.to_vec(),
    })?)
    .await
    .map_err(|_| ())?;
    let reader = async {
        loop {
            let incoming = select(
                Box::pin(stream.next()),
                Box::pin(gloo_timers::future::TimeoutFuture::new(25000)),
            )
            .await;
            let Either::Left((Some(Ok(Message::Text(text))), _)) = incoming else {
                return Err(());
            };
            let frame: ServerFrame = serde_json::from_str(&text).map_err(|_| ())?;
            if frame.version != VERSION {
                return Err(());
            }
            match frame.event {
                ServerEvent::Ready { preferences } => {
                    apply(ui, preferences);
                    ui.notifications.connected.set(true);
                }
                ServerEvent::Alert { alert } => {
                    let replayed = seen.contains(&alert.id);
                    if !replayed {
                        seen.push_back(alert.id.clone());
                        if seen.len() > 256 {
                            seen.pop_front();
                        }
                    }
                    // Reconnect clears stale presentation. Restore a still-ringing
                    // call's controls without playing its sound a second time.
                    if !replayed || alert.kind == Kind::IncomingCall {
                        receive(ui, alert, replayed);
                    }
                }
                ServerEvent::Pong {} => {}
                ServerEvent::Calls { ringing } => sync_calls(ui, ringing).await,
                ServerEvent::Revoked {} => return Err(()),
            }
        }
    };
    let writer = async {
        loop {
            gloo_timers::future::TimeoutFuture::new(10000).await;
            sink.send(encode(ClientEvent::Ping {})?)
                .await
                .map_err(|_| ())?;
        }
    };
    match select(Box::pin(reader), Box::pin(writer)).await {
        Either::Left((result, _)) | Either::Right((result, _)) => result,
    }
}

#[component]
fn Controls(
    delivery: Signal<Delivery>,
    disabled: Signal<bool>,
    change: Callback<Delivery>,
    #[prop(default = false)] mentions: bool,
) -> impl IntoView {
    view! {<div class="flex flex-wrap items-center gap-4">
        <label>"Receive "<select class="rounded bg-surface p-2" prop:value=move ||match delivery.get().mode { Mode::All=>"all",Mode::Mentions=>"mentions",Mode::Off=>"off" } disabled=move ||disabled.get() on:change=move |e|change.run(Delivery { mode:match event_target_value(&e).as_str(){"mentions"=>Mode::Mentions,"off"=>Mode::Off,_=>Mode::All},..delivery.get_untracked() })><option value="all">"All activity"</option><Show when=move ||mentions><option value="mentions">"Mentions only"</option></Show><option value="off">"Nothing"</option></select></label>
        <label class="flex items-center gap-2"><input type="checkbox" prop:checked=move ||delivery.get().sound disabled=move ||disabled.get() on:change=move |e|change.run(Delivery {sound:event_target_checked(&e),..delivery.get_untracked()})/>"Sound"</label>
        <label class="flex items-center gap-2"><input type="checkbox" prop:checked=move ||delivery.get().banner disabled=move ||disabled.get() on:change=move |e|change.run(Delivery {banner:event_target_checked(&e),..delivery.get_untracked()})/>"Banner"</label>
        <label class="flex items-center gap-2">"Volume"<input aria-label="Notification volume" type="range" min="0" max="100" prop:value=move ||delivery.get().volume disabled=move ||disabled.get() on:change=move |e|if let Ok(volume)=event_target_value(&e).parse(){change.run(Delivery {volume,..delivery.get_untracked()});}/><span>{move ||format!("{}%",delivery.get().volume)}</span></label>
    </div>}
}
#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {<div class="space-y-4">
        <div class="settings-card"><p>"Choose what reaches you, and how. Changes are saved to your account and apply to every connected device."</p><p class="text-sm text-white/60">"Right-click a channel or conversation to mute it or choose its own settings. Mentions only is available for server messages."</p><p role="status">{move ||if ui.notifications.connected.get(){"Notifications connected"}else{"Reconnecting notifications…"}}</p><p role="alert" class="text-red-200">{move ||ui.notifications.error.get()}</p></div>
        {Kind::ALL.into_iter().map(|kind| {
            let delivery=Signal::derive(move ||ui.notifications.preferences.get().defaults.iter().find(|r|r.kind==kind).map_or_else(||Delivery::default_for(kind),|r|r.delivery));
            view!{<section class="settings-card"><h3 class="mb-3 font-semibold">{kind.label()}</h3><p class="mb-3 text-sm text-white/60">{thiscord_frontend::notification_sound::description(kind)}</p><Controls delivery=delivery mentions=matches!(kind,Kind::GuildMessage) disabled=Signal::derive(move ||ui.notifications.saving.get()||!ui.notifications.connected.get()) change=Callback::new(move |delivery|request(ui,Request::SetDefault {revision:ui.notifications.preferences.get_untracked().revision,kind,delivery}))/><button class="mt-3 text-sm text-brand" on:click=move |_|sound(ui,kind,delivery.get_untracked().volume)>"Preview sound"</button></section>}
        }).collect_view()}
        <section class="settings-card"><h3 class="font-semibold">"Custom subscriptions"</h3><For each=move ||ui.notifications.preferences.get().subscriptions key=|s|format!("{:?}",s.target) children=move |s| {
            let target=s.target; view!{<button class="mt-2 block text-sm text-brand" on:click=move |_|edit(ui,target,target_name(ui,target))>{target_name(ui,target)}</button>}
        }/></section>
    </div>}
}
fn target_name(ui: Ui, target: Target) -> String {
    match target {
        Target::Updates => "Application updates".into(),
        Target::Conversation { conversation_id } => ui
            .friends
            .snapshot
            .get_untracked()
            .friends
            .into_iter()
            .find(|f| f.conversation_id == conversation_id)
            .map(|f| f.person.display_name)
            .unwrap_or_else(|| "Direct conversation".into()),
        Target::Channel { channel_id, .. } => ui
            .server
            .get_untracked()
            .and_then(|s| s.channels.into_iter().find(|c| c.id == channel_id))
            .map(|c| c.name)
            .unwrap_or_else(|| format!("Channel {channel_id}")),
    }
}
#[component]
fn Editor(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        let open = ui.notifications.editor.get().is_some();
        if let Some(d) = dialog.get() {
            if open {
                let _ = d.show_modal();
            } else {
                d.close();
            }
        }
    });
    view! {<dialog node_ref=dialog class="context-dialog max-w-xl space-y-4" aria-label="Subscription notifications" on:close=move |_|ui.notifications.editor.set(None) on:click=move |e|super::dialog::dismiss_outside(e,dialog,false)>
        <h2 class="text-xl font-semibold">{move ||ui.notifications.editor.get().map(|(_,name)|format!("Notifications · {name}"))}</h2>
        {move ||ui.notifications.editor.get().map(|(target,_)| {
            let kind=match target {Target::Conversation {..}=>Kind::DirectMessage,Target::Updates=>Kind::Update,Target::Channel {channel_id,..}=>if ui.server.get_untracked().is_some_and(|s|s.channels.iter().any(|c|c.id==channel_id && c.kind==thiscord_shared::permissions::ChannelKind::Voice)){Kind::VoiceJoin}else{Kind::GuildMessage}};
            let delivery=Signal::derive(move ||{let p=ui.notifications.preferences.get();p.subscriptions.iter().find(|s|s.target==target).and_then(|s|s.delivery).unwrap_or_else(||p.defaults.iter().find(|r|r.kind==kind).map_or_else(||Delivery::default_for(kind),|r|r.delivery))});
            view!{<p class="text-sm text-white/60">"This override applies to all notifications from this source. Use defaults to keep separate message, call and voice preferences."</p><Controls delivery=delivery mentions=matches!(kind,Kind::GuildMessage) disabled=Signal::derive(move ||ui.notifications.saving.get()||!ui.notifications.connected.get()) change=Callback::new(move |delivery|request(ui,Request::SetSubscription {revision:ui.notifications.preferences.get_untracked().revision,target,delivery:Some(delivery)}))/>
                <button class="text-brand" disabled=move ||ui.notifications.saving.get() on:click=move |_|request(ui,Request::SetSubscription {revision:ui.notifications.preferences.get_untracked().revision,target,delivery:None})>"Use event defaults"</button>
                <button class="block text-brand" disabled=move ||ui.notifications.saving.get() on:click=move |_|request(ui,Request::RemoveSubscription {revision:ui.notifications.preferences.get_untracked().revision,target})>"Remove saved settings and mute"</button>
                <p>{move ||if muted(ui,target){"This subscription is muted."}else{"This subscription is not muted."}}</p>
                <div class="flex flex-wrap gap-2">{Snooze::OPTIONS.into_iter().map(|(duration,label)|view!{<button class="rounded bg-white/10 px-3 py-2 text-sm" disabled=move ||ui.notifications.saving.get() on:click=move |_|snooze(ui,target,duration)>{label}</button>}).collect_view()}</div>
            }
        })}
        <p role="alert" class="text-sm text-red-200">{move ||ui.notifications.error.get()}</p><button class="rounded bg-brand px-4 py-2" on:click=move |_|ui.notifications.editor.set(None)>"Done"</button>
    </dialog>}
}
