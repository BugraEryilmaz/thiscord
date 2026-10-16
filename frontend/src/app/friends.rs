use super::{
    Field, Ui,
    icons::{Glyph, Icon},
};
use crate::account_client;
use futures_util::{
    SinkExt, StreamExt,
    future::{AbortHandle, Abortable, Either, select},
};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;
use thiscord_shared::{
    ClientMessageId,
    friends::*,
    ids::{CallId, ConversationId},
};

#[derive(Clone, Copy)]
pub(super) struct State {
    pub snapshot: RwSignal<Snapshot>,
    pub selected: RwSignal<Option<ConversationId>>,
    connected: RwSignal<bool>,
    intent: RwSignal<Option<CallId>>,
    intent_at: RwSignal<f64>,
    joined: RwSignal<Option<CallId>>,
}
impl State {
    pub fn new() -> Self {
        Self {
            snapshot: RwSignal::new(Default::default()),
            selected: RwSignal::new(None),
            connected: RwSignal::new(false),
            intent: RwSignal::new(None),
            intent_at: RwSignal::new(0.0),
            joined: RwSignal::new(None),
        }
    }
}
async fn request(ui: Ui, command: &Request) -> Result<Response, String> {
    let token = ui.token.get_untracked();
    let response = account_client::api_request(FRIENDS_PATH, command, token.as_deref()).await;
    if token != ui.token.get_untracked() {
        return Err("Session changed".into());
    }
    response
}
fn action(ui: Ui, command: Request) {
    leptos::task::spawn_local(async move {
        match request(ui, &command).await {
            Ok(Response::Call { call }) => {
                ui.friends.intent.set(Some(call.id));
                ui.friends.intent_at.set(js_sys::Date::now());
                ui.friends.selected.set(Some(call.conversation_id));
            }
            Ok(_) => {}
            Err(error) => ui.status.set(error),
        }
    });
}
pub(super) fn end_call(ui: Ui) {
    if let Some(id) = ui.friends.intent.get_untracked() {
        ui.friends.intent.set(None);
        ui.friends.joined.set(None);
        action(ui, Request::EndCall { call_id: id });
    }
}
fn open(ui: Ui, id: ConversationId) {
    ui.friends.selected.set(Some(id));
    ui.server.set(None);
    ui.page.set("servers");
}
fn available(ui: Ui) -> bool {
    account_client::desktop() || ui.audio_control.is_remote()
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let abort = StoredValue::new(None::<AbortHandle>);
    on_cleanup(move || {
        if let Some(a) = abort.get_value() {
            a.abort();
        }
    });
    Effect::new(move |_| {
        let token = ui.token.get();
        if let Some(a) = abort.get_value() {
            a.abort();
        }
        ui.friends.snapshot.set(Default::default());
        ui.friends.selected.set(None);
        ui.friends.connected.set(false);
        ui.friends.intent.set(None);
        ui.friends.joined.set(None);
        let Some(token) = token else {
            return;
        };
        let (handle, registration) = AbortHandle::new_pair();
        abort.set_value(Some(handle));
        leptos::task::spawn_local(async move {
            let _ = Abortable::new(
                async move {
                    let mut delay = 1000;
                    loop {
                        let started = js_sys::Date::now();
                        let _ = connected(ui, &token).await;
                        ui.friends.connected.set(false);
                        ui.friends.snapshot.update(|s| {
                            for f in &mut s.friends {
                                f.online = false;
                            }
                        });
                        if js_sys::Date::now() - started > 10000.0 {
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
    view! {<div class="fixed right-6 top-14 z-40 max-w-sm space-y-2" aria-live="polite">
        <For each=move ||ui.friends.snapshot.get().calls key=|c|c.id children=move |initial| {
            let id=initial.id;
            let call=Signal::derive(move ||ui.friends.snapshot.get().calls.into_iter().find(|c|c.id==id).unwrap_or_else(||initial.clone()));
            let label=move ||ui.friends.snapshot.get().friends.into_iter().find(|f|f.conversation_id==call.get().conversation_id).map(|f|f.person.display_name).unwrap_or_else(||"Friend".into());
            let incoming=move ||call.get().state==CallState::Ringing && ui.account.get().is_some_and(|a|a.id==call.get().callee);
            view! {<section class="rounded-xl border border-white/15 bg-surface p-4 shadow-2xl">
                <button class="font-semibold hover:underline" on:click=move |_|open(ui,call.get_untracked().conversation_id)>{label}</button>
                <p class="mb-3 text-sm text-white/60">{move ||if incoming(){"Incoming voice call"}else if call.get().state==CallState::Ringing{"Calling…"}else{"Voice call"}}</p>
                <div class="flex flex-wrap gap-2">
                    <Show when=incoming><button class="rounded-lg bg-emerald-600 px-4 py-2 disabled:opacity-50" disabled=move ||!available(ui) || ui.voice_joining.get() on:click=move |_|action(ui,Request::AcceptCall {call_id:id})>"Accept"</button></Show>
                    <Show when=move ||call.get().state==CallState::Active && ui.voice.get().channel_id!=Some(call.get().voice_channel())><button class="rounded-lg bg-brand px-4 py-2 disabled:opacity-50" disabled=move ||!available(ui) || ui.voice_joining.get() on:click=move |_| {ui.friends.joined.set(None);ui.friends.intent.set(Some(id));ui.friends.intent_at.set(js_sys::Date::now());join_call(ui,call.get_untracked());}>"Join call"</button></Show>
                    <button class="rounded-lg bg-red-500/20 px-4 py-2 text-red-200" on:click=move |_| {
                        action(ui,Request::EndCall {call_id:id});
                        if ui.friends.intent.get_untracked()==Some(id) {ui.friends.intent.set(None);ui.friends.joined.set(None);super::audio::leave(ui);}
                    }>{move ||if incoming(){"Decline"}else{"End call"}}</button>
                </div>
                <Show when=move ||!available(ui)><p class="mt-2 text-xs text-white/50">"Open the desktop app to use voice."</p></Show>
            </section>}
        }/>
    </div>}
}
fn join_call(ui: Ui, call: Call) {
    if ui.friends.joined.get_untracked() == Some(call.id) || ui.voice_joining.get_untracked() {
        return;
    }
    ui.friends.joined.set(Some(call.id));
    let name = ui
        .friends
        .snapshot
        .get_untracked()
        .friends
        .into_iter()
        .find(|f| f.conversation_id == call.conversation_id)
        .map(|f| f.person.display_name)
        .unwrap_or_else(|| "Friend".into());
    super::audio::join_location(
        ui,
        super::audio::VoiceLocation {
            guild_id: call.voice_scope(),
            channel_id: call.voice_channel(),
            guild_name: "Private call".into(),
            channel_name: name,
        },
    );
}
async fn connected(ui: Ui, token: &str) -> Result<(), ()> {
    let base = account_client::api_base()
        .trim_end_matches('/')
        .replacen("http", "ws", 1);
    let (mut sink, mut stream) = WebSocket::open(&format!("{base}{FRIENDS_SOCKET_PATH}"))
        .map_err(|_| ())?
        .split();
    sink.send(Message::Text(
        serde_json::to_string(&ClientFrame {
            version: VERSION,
            event: ClientEvent::Connect {
                token: token.into(),
            },
        })
        .map_err(|_| ())?,
    ))
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
                ServerEvent::Snapshot { mut snapshot } => {
                    ui.friends.connected.set(true);
                    // A late selection response must never appear in another DM.
                    if snapshot.conversation_id != ui.friends.selected.get_untracked() {
                        snapshot.history = None;
                    }
                    let intent = ui.friends.intent.get_untracked();
                    let active = snapshot
                        .calls
                        .iter()
                        .find(|c| Some(c.id) == intent)
                        .cloned();
                    if ui.friends.selected.get_untracked().is_some_and(|id| {
                        !snapshot.friends.iter().any(|f| {
                            f.conversation_id == id && f.relationship == Relationship::Friend
                        })
                    }) {
                        ui.friends.selected.set(None);
                    }
                    ui.friends.snapshot.set(snapshot);
                    if let Some(call) = active {
                        if call.state == CallState::Active {
                            join_call(ui, call);
                        }
                    } else if intent.is_some()
                        && js_sys::Date::now() - ui.friends.intent_at.get_untracked() > 5000.0
                    {
                        let joined = ui.friends.joined.get_untracked();
                        ui.friends.intent.set(None);
                        ui.friends.joined.set(None);
                        if joined.is_some_and(|id| {
                            ui.voice
                                .get_untracked()
                                .channel_id
                                .is_some_and(|c| c.as_uuid() == id.as_uuid())
                        }) {
                            super::audio::leave(ui);
                        }
                    }
                }
                ServerEvent::Pong {} => {}
                _ => return Err(()),
            }
        }
    };
    let writer = async {
        let mut selected = None;
        let mut ticks = 0u32;
        loop {
            gloo_timers::future::TimeoutFuture::new(100).await;
            ticks = ticks.wrapping_add(1);
            let desired = if ui.server.get_untracked().is_none() {
                ui.friends.selected.get_untracked()
            } else {
                None
            };
            let event = if selected != Some(desired) {
                selected = Some(desired);
                Some(ClientEvent::Select {
                    conversation_id: desired,
                })
            } else if ticks.is_multiple_of(100) {
                Some(ClientEvent::Ping {})
            } else {
                None
            };
            if let Some(event) = event {
                sink.send(Message::Text(
                    serde_json::to_string(&ClientFrame {
                        version: VERSION,
                        event,
                    })
                    .map_err(|_| ())?,
                ))
                .await
                .map_err(|_| ())?;
            }
        }
    };
    match select(Box::pin(reader), Box::pin(writer)).await {
        Either::Left((r, _)) | Either::Right((r, _)) => r,
    }
}

#[component]
pub(super) fn Home(ui: Ui) -> impl IntoView {
    let filter = RwSignal::new("online");
    view! {<section class="flex h-full min-h-0 flex-col pb-[var(--dock-height)] lg:pb-0">
        <header class="flex flex-wrap items-center gap-4 border-b border-white/10 px-6 py-4">
            <button class="flex items-center gap-2 text-lg font-bold" on:click=move |_|ui.friends.selected.set(None)><Icon kind=Glyph::Person/>"Friends"</button>
            <nav class="flex gap-2" aria-label="Friend filters">
                {[("online","Online"),("all","All"),("pending","Requests")].into_iter().map(|(value,label)|view!{<button class="rounded-lg px-3 py-2 text-sm hover:bg-white/10" class:bg-brand=move ||filter.get()==value && ui.friends.selected.get().is_none() aria-pressed=move ||filter.get()==value && ui.friends.selected.get().is_none() on:click=move |_|{filter.set(value);ui.friends.selected.set(None);}>{label}</button>}).collect_view()}
            </nav>
            <div class="ml-auto"><AddFriend ui=ui/></div>
        </header>
        <div class="flex min-h-0 flex-1">
            <aside class="hidden w-56 shrink-0 overflow-y-auto border-r border-white/10 p-4 pb-[var(--dock-height)] lg:block" aria-label="Direct messages">
                <h3 class="mb-3 text-xs font-semibold uppercase tracking-wider text-white/40">"Direct messages"</h3>
                <For each=move ||{ui.friends.snapshot.get().friends.into_iter().filter(|f|f.relationship==Relationship::Friend).collect::<Vec<_>>()} key=|f|f.conversation_id children=move |f| {
                    let id=f.conversation_id;
                    let friend=Signal::derive(move ||ui.friends.snapshot.get().friends.into_iter().find(|f|f.conversation_id==id).unwrap_or_else(||f.clone()));
                    view!{<button class="mb-1 flex w-full items-center gap-2 rounded-lg p-2 text-left hover:bg-white/5" class=("bg-white/10",move ||ui.friends.selected.get()==Some(id)) on:click=move |_|open(ui,id)><span class="size-2 shrink-0 rounded-full" class:bg-emerald-400=move ||friend.get().online class=("bg-white/30",move ||!friend.get().online)></span><span class="truncate">{move ||friend.get().person.display_name}</span></button>}
                }/>
            </aside>
            <div class="min-h-0 min-w-0 flex-1">
                <Show when=move ||ui.friends.selected.get().is_some() fallback=move ||view!{<FriendList ui=ui filter=filter/>}>
                    <For each=move ||{ui.friends.selected.get().into_iter().collect::<Vec<_>>()} key=|id|*id children=move |id|view!{<Conversation ui=ui id=id/>}/>
                </Show>
            </div>
        </div>
    </section>}
}
#[component]
fn FriendList(ui: Ui, filter: RwSignal<&'static str>) -> impl IntoView {
    let friends = Memo::new(move |_| {
        ui.friends
            .snapshot
            .get()
            .friends
            .into_iter()
            .filter(|f| match filter.get() {
                "online" => f.relationship == Relationship::Friend && f.online,
                "all" => f.relationship == Relationship::Friend,
                _ => f.relationship != Relationship::Friend,
            })
            .collect::<Vec<_>>()
    });
    view! {<div class="h-full overflow-y-auto p-6">
        <p class="mb-5 text-xs font-semibold uppercase tracking-wider text-white/45">{move ||format!("{} — {}",if filter.get()=="pending"{"Requests"}else if filter.get()=="online"{"Online friends"}else{"All friends"},friends.get().len())}</p>
        <Show when=move ||!ui.friends.connected.get()><p role="status" class="mb-4 text-sm text-amber-200">"Connecting to friends…"</p></Show>
        <Show when=move ||friends.get().is_empty()><div class="flex min-h-60 flex-col items-center justify-center gap-3 text-center"><span class="rounded-full bg-brand/10 p-5 text-brand"><Icon kind=Glyph::Person/></span><h2 class="text-xl font-semibold">{move ||if filter.get()=="pending"{"No pending requests"}else if filter.get()=="online"{"No friends online right now"}else{"Make yourself at home"}}</h2><p class="max-w-sm text-sm text-white/50">"Use Add friend to find someone by username. Your conversations will appear here."</p></div></Show>
        <For each=move ||friends.get() key=|f|f.conversation_id children=move |initial| {
            let id=initial.conversation_id;let person=initial.person.id;
            let f=Signal::derive(move ||ui.friends.snapshot.get().friends.into_iter().find(|f|f.conversation_id==id).unwrap_or_else(||initial.clone()));
            view!{<div class="flex flex-wrap items-center gap-4 border-t border-white/10 py-4">
                <crate::avatar::Avatar class="size-10 text-sm" name=Signal::derive(move ||f.get().person.display_name) avatar_id=Signal::derive(move ||f.get().person.avatar_id)/>
                <div class="min-w-0 flex-1"><p class="truncate font-semibold">{move ||f.get().person.display_name}<span class="ml-2 text-sm font-normal text-white/40">{move ||format!("@{}",f.get().person.username)}</span></p><p class="text-xs text-white/50">{move ||match f.get().relationship {Relationship::Incoming=>"Incoming request",Relationship::Outgoing=>"Request sent",Relationship::Friend if f.get().online=>"Online",Relationship::Friend=>"Offline"}}</p></div>
                <Show when=move ||f.get().relationship==Relationship::Friend>
                    <button class="icon-button" aria-label="Message friend" data-tip="Message" on:click=move |_|open(ui,id)><Icon kind=Glyph::Send/></button>
                    <button class="icon-button disabled:opacity-40" aria-label="Call friend" data-tip="Voice call (desktop)" disabled=move ||!available(ui) || !ui.friends.connected.get() || !ui.friends.snapshot.get().calls.is_empty() on:click=move |_|action(ui,Request::StartCall {conversation_id:id})><Icon kind=Glyph::Headphones/></button>
                </Show>
                <Show when=move ||f.get().relationship==Relationship::Incoming><button class="rounded-lg bg-emerald-600 px-3 py-2 text-sm" on:click=move |_|action(ui,Request::Accept {account_id:person})>"Accept"</button></Show>
                <button class="rounded-lg px-3 py-2 text-sm text-white/50 hover:bg-red-500/10 hover:text-red-200" on:click=move |_|{
                    if f.get_untracked().relationship!=Relationship::Friend || window().confirm_with_message("Remove this friend? Private messages and calls will be unavailable until you become friends again.").unwrap_or(false) {action(ui,Request::Remove {account_id:person});}
                }>{move ||match f.get().relationship {Relationship::Incoming=>"Decline",Relationship::Outgoing=>"Cancel",Relationship::Friend=>"Remove"}}</button>
            </div>}
        }/>
    </div>}
}
#[component]
fn AddFriend(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let query = RwSignal::new(String::new());
    let people = RwSignal::new(Vec::<Person>::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let searched = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = request(
                ui,
                &Request::Search {
                    query: query.get_untracked(),
                },
            )
            .await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            searched.set(true);
            match result {
                Ok(Response::People { people: found }) => people.set(found),
                Err(e) => {
                    people.set(vec![]);
                    error.set(e);
                }
                _ => {}
            }
        });
    };
    view! {<button class="flex items-center gap-2 rounded-lg bg-brand px-4 py-2 text-sm font-semibold" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}><Icon kind=Glyph::Plus/>"Add friend"</button>
        <dialog node_ref=dialog aria-labelledby="add-friend-title" class="m-auto w-[calc(100%-2rem)] max-w-lg rounded-2xl border border-white/15 bg-surface p-6 text-white shadow-2xl backdrop:bg-black/70" on:click=move |ev|super::dialog::dismiss_outside(ev,dialog,false)>
            <header class="mb-2 flex items-center justify-between"><h2 id="add-friend-title" class="text-xl font-bold">"Add friend"</h2><button class="icon-button" aria-label="Close add friend" on:click=move |_|if let Some(d)=dialog.get(){d.close();}><Icon kind=Glyph::Close/></button></header>
            <p class="mb-5 text-sm text-white/50">"Search by username and send a friend request."</p>
            <Show when=move ||ui.account.get().is_some_and(|a|!a.email_verified)><p class="mb-4 text-sm text-amber-200">"Verify your email in account settings before sending friend requests."</p></Show>
            <form class="flex items-end gap-3" on:submit=submit><div class="min-w-0 flex-1"><Field label="Username" value=query/></div><button class="rounded-lg bg-brand px-4 py-3 disabled:opacity-50" disabled=move ||busy.get()>{move ||if busy.get(){"Searching…"}else{"Search"}}</button></form>
            <p role="status" class="my-3 text-sm text-red-200">{move ||error.get()}</p>
            <Show when=move ||searched.get() && people.get().is_empty() && error.get().is_empty()><p class="py-4 text-sm text-white/50">"No matching usernames. Try another search."</p></Show>
            <ul class="max-h-80 space-y-3 overflow-y-auto"><For each=move ||people.get() key=|p|p.id children=move |person| {
                let id=person.id;let sending=RwSignal::new(false);let sent=RwSignal::new(false);
                let relation=move ||ui.friends.snapshot.get().friends.into_iter().find(|f|f.person.id==id).map(|f|f.relationship);
                view!{<li class="flex items-center gap-3 rounded-lg bg-white/5 p-3"><div class="min-w-0 flex-1"><p class="truncate font-semibold">{person.display_name}</p><p class="text-xs text-white/40">{format!("@{}",person.username)}</p></div><button class="rounded-lg bg-brand px-3 py-2 text-sm disabled:bg-white/10 disabled:text-white/50" disabled=move ||sending.get() || sent.get() || relation().is_some() || !ui.account.get().is_some_and(|a|a.email_verified) on:click=move |_|{
                    sending.set(true);leptos::task::spawn_local(async move{let result=request(ui,&Request::Add {account_id:id}).await;if sending.is_disposed(){return;}sending.set(false);match result {Ok(_)=>sent.set(true),Err(e)=>error.set(e)}});
                }>{move ||match relation(){Some(Relationship::Friend)=>"Friends",Some(Relationship::Incoming)=>"Incoming request",Some(Relationship::Outgoing)=>"Request sent",None if sent.get()=>"Request sent",None if sending.get()=>"Sending…",None=>"Add friend"}}</button></li>}
            }/></ul>
        </dialog>
    }
}

#[component]
fn Conversation(ui: Ui, id: ConversationId) -> impl IntoView {
    let messages = RwSignal::new(Vec::<DirectMessage>::new());
    let older = RwSignal::new(None);
    let draft = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let retry = RwSignal::new(None::<(ClientMessageId, String)>);
    let list = NodeRef::<leptos::html::Div>::new();
    let bottom = RwSignal::new(true);
    let past = RwSignal::new(false);
    let friend = Signal::derive(move || {
        ui.friends
            .snapshot
            .get()
            .friends
            .into_iter()
            .find(|f| f.conversation_id == id)
    });
    Effect::new(move |_| {
        let snapshot = ui.friends.snapshot.get();
        if past.get() || snapshot.conversation_id != Some(id) {
            return;
        }
        if let Some(history) = snapshot.history {
            let changed = messages.with_untracked(|m| *m != history.messages);
            older.set(history.older);
            if changed {
                // The live window is a complete bounded snapshot, so reconnect
                // cannot leave a gap after more than one page of missed messages.
                messages.set(history.messages);
                if bottom.get_untracked() {
                    request_animation_frame(move || {
                        if let Some(node) = list.get_untracked() {
                            node.set_scroll_top(node.scroll_height());
                        }
                    });
                }
            }
        }
    });
    let send = move || {
        if busy.get_untracked() {
            return;
        }
        let payload = retry.get_untracked().or_else(|| {
            window()
                .crypto()
                .ok()?
                .random_uuid()
                .parse()
                .ok()
                .map(|id| (id, draft.get_untracked()))
        });
        let Some((client_id, content)) = payload else {
            error.set("Could not create a message ID".into());
            return;
        };
        if content.trim().is_empty() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        retry.set(Some((client_id, content.clone())));
        leptos::task::spawn_local(async move {
            let result = request(
                ui,
                &Request::Send {
                    conversation_id: id,
                    client_id,
                    content,
                },
            )
            .await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(Response::Message { message }) => {
                    messages.update(|m| {
                        if !m.iter().any(|old| old.id == message.id) {
                            m.push(message);
                            m.sort_by_key(|m| (m.created_at, m.id));
                        }
                    });
                    past.set(false);
                    draft.set(String::new());
                    retry.set(None);
                    bottom.set(true);
                    request_animation_frame(move || {
                        if let Some(node) = list.get_untracked() {
                            node.set_scroll_top(node.scroll_height());
                        }
                    });
                }
                Err(e) => error.set(e),
                _ => {}
            }
        });
    };
    view! {<section class="flex h-full min-h-0 flex-col">
        <header class="flex items-center gap-3 border-b border-white/10 px-6 py-4"><button class="icon-button lg:hidden" aria-label="Back to friends" on:click=move |_|ui.friends.selected.set(None)><Icon kind=Glyph::Close/></button><h2 class="min-w-0 flex-1 truncate font-semibold">{move ||friend.get().map(|f|f.person.display_name).unwrap_or_default()}</h2><span class="text-xs text-white/50">{move ||if friend.get().is_some_and(|f|f.online){"Online"}else{"Offline"}}</span><button class="icon-button disabled:opacity-40" aria-label="Start private voice call" data-tip="Voice call (desktop)" disabled=move ||!available(ui) || !ui.friends.snapshot.get().calls.is_empty() on:click=move |_|action(ui,Request::StartCall {conversation_id:id})><Icon kind=Glyph::Headphones/></button></header>
        <div node_ref=list class="min-h-0 flex-1 overflow-y-auto px-6 py-4" on:scroll=move |_|if let Some(node)=list.get(){bottom.set(node.scroll_height()-node.scroll_top()-node.client_height()<80);}>
            <Show when=move ||older.get().is_some()><button class="mb-4 text-sm text-brand disabled:opacity-50" disabled=move ||loading.get() on:click=move |_| {
                let before=older.get_untracked();if loading.get_untracked(){return;}loading.set(true);

                leptos::task::spawn_local(async move {let result=request(ui,&Request::History {conversation_id:id,before,limit:Default::default()}).await;if loading.is_disposed(){return;}loading.set(false);match result {Ok(Response::History {history})=>{past.set(true);older.set(history.older);messages.set(history.messages);request_animation_frame(move ||if let Some(n)=list.get_untracked(){n.set_scroll_top(0);});},Err(e)=>error.set(e),_=>{}}});
            }>{move ||if loading.get(){"Loading…"}else{"Older messages"}}</button></Show>
            <Show when=move ||past.get()><button class="mb-4 ml-4 text-sm text-brand" on:click=move |_|{bottom.set(true);past.set(false);}>"Jump to latest"</button></Show>
            <Show when=move ||messages.get().is_empty()><div class="py-12 text-center"><h3 class="text-xl font-semibold">"Start a conversation"</h3><p class="mt-2 text-sm text-white/45">"Only you and your friend can access this conversation."</p></div></Show>
            <ol class="space-y-5" aria-label="Private messages"><For each=move ||messages.get() key=|m|m.id children=move |m| {
                let own=ui.account.get_untracked().is_some_and(|a|a.id==m.author_id);
                view!{<li><div class="flex items-baseline gap-3"><strong class="text-sm">{if own {"You".into()}else{friend.get_untracked().map(|f|f.person.display_name).unwrap_or_else(||"Friend".into())}}</strong><time class="text-xs text-white/35" datetime=m.created_at.to_rfc3339()>{m.created_at.format("%b %d, %H:%M UTC").to_string()}</time></div><p class="mt-1 whitespace-pre-wrap break-words text-white/85">{m.content}</p></li>}
            }/></ol>
        </div>
        <form class="shrink-0 border-t border-white/10 p-4" on:submit=move |ev|{ev.prevent_default();send();}>
            <p role="alert" class="mb-2 text-sm text-red-200">{move ||error.get()}</p>
            <div class="flex items-end gap-3 rounded-xl bg-white/5 p-3"><textarea class="max-h-40 min-h-12 flex-1 resize-y bg-transparent p-2 outline-none disabled:opacity-50" aria-label="Private message" placeholder="Message your friend…" maxlength="4000" prop:value=move ||draft.get() disabled=move ||busy.get() || retry.get().is_some() on:input=move |ev|draft.set(event_target_value(&ev))></textarea><button class="rounded-lg bg-brand p-3 disabled:opacity-40" aria-label="Send private message" disabled=move ||busy.get() || !ui.friends.connected.get() || draft.get().trim().is_empty()>{move ||if busy.get(){"Sending…"}else if retry.get().is_some(){"Retry"}else{"Send"}}</button></div>
            <Show when=move ||retry.get().is_some() && !busy.get()><button type="button" class="mt-2 text-xs text-white/50" on:click=move |_|{retry.set(None);error.set(String::new());}>"Discard retry and edit draft"</button></Show>
        </form>
    </section>}
}
