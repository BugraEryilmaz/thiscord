use super::Ui;
use futures_util::{
    SinkExt, StreamExt,
    future::{AbortHandle, Abortable},
};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;
use thiscord_shared::{
    ChannelId, ClientMessageId, GuildId, MessageId,
    chat::*,
    pagination::PageCursor,
    permissions::{Permission, Permissions},
};

#[derive(Clone)]
struct Pending {
    id: ClientMessageId,
    content: String,
    failed: bool,
}
#[derive(Clone, Copy)]
pub(super) struct Chat {
    ui: Ui,
    epoch: u64,
    guild: GuildId,
    channel: ChannelId,
    messages: RwSignal<Vec<ChatMessage>>,
    pending: RwSignal<Vec<Pending>>,
    status: RwSignal<String>,
    online: RwSignal<Vec<OnlineMember>>,
    older: RwSignal<Option<PageCursor>>,
    permissions: RwSignal<Permissions>,
    list: NodeRef<leptos::html::Div>,
    bottom: RwSignal<bool>,
    typing: RwSignal<bool>,
    last_read: RwSignal<i64>,
}
impl Chat {
    fn alive(self) -> bool {
        self.ui.chat_epoch.get_untracked() == self.epoch
    }
    async fn request(self, command: &ChatRequest) -> Result<ChatResponse, String> {
        let token = self.ui.token.get_untracked();
        match futures_util::future::select(
            Box::pin(crate::account_client::api_request(
                CHAT_PATH,
                command,
                token.as_deref(),
            )),
            Box::pin(gloo_timers::future::TimeoutFuture::new(15000)),
        )
        .await
        {
            futures_util::future::Either::Left((result, _)) => result,
            _ => Err("Request timed out; retry safely with the same message ID".into()),
        }
    }
    fn scroll(self) {
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(0).await;
            if self.alive()
                && self.bottom.get_untracked()
                && let Some(node) = self.list.get()
            {
                node.set_scroll_top(node.scroll_height());
            }
        });
    }
    fn merge(self, message: ChatMessage) {
        self.pending
            .update(|p| p.retain(|p| p.id != message.client_id));
        self.messages.update(|messages| {
            if let Some(old) = messages.iter_mut().find(|m| m.id == message.id) {
                if message.revision >= old.revision {
                    *old = message;
                }
            } else {
                messages.push(message);
                messages.sort_by_key(|m| m.sequence);
            }
        });
        self.scroll();
    }
    fn read(self) {
        if !self.bottom.get_untracked() || !document().has_focus().unwrap_or(false) {
            return;
        }
        let through = self
            .messages
            .get_untracked()
            .last()
            .map(|m| m.sequence)
            .unwrap_or(0);
        if through <= self.last_read.get_untracked() {
            return;
        }
        self.last_read.set(through);
        leptos::task::spawn_local(async move {
            if self
                .request(&ChatRequest::Read {
                    guild_id: self.guild,
                    channel_id: self.channel,
                    through,
                })
                .await
                .is_err()
                && self.alive()
            {
                self.last_read.set(0);
            }
        });
    }
    fn send(self, pending: Pending) {
        self.pending.update(|items| {
            if let Some(p) = items.iter_mut().find(|p| p.id == pending.id) {
                p.failed = false;
            } else {
                items.push(pending.clone());
            }
        });
        self.bottom.set(true);
        self.scroll();
        leptos::task::spawn_local(async move {
            let response = self
                .request(&ChatRequest::Send {
                    guild_id: self.guild,
                    channel_id: self.channel,
                    client_id: pending.id,
                    content: pending.content,
                })
                .await;
            if !self.alive() {
                return;
            }
            match response {
                Ok(ChatResponse::Message { message }) => {
                    self.status.set("Message sent".into());
                    self.merge(message);
                    self.read();
                }
                Err(error) => {
                    self.status.set(error);
                    self.pending.update(|items| {
                        if let Some(p) = items.iter_mut().find(|p| p.id == pending.id) {
                            p.failed = true;
                        }
                    });
                }
                _ => {}
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Subscription {
    guild: Option<GuildId>,
    channel: Option<ChannelId>,
    epoch: u64,
}
fn desired(ui: Ui) -> Subscription {
    let guild = ui.server.get_untracked().map(|s| s.guild.id);
    let chat = ui
        .active_chat
        .get_untracked()
        .filter(|c| c.alive() && Some(c.guild) == guild);
    Subscription {
        guild,
        channel: chat.map(|c| c.channel),
        epoch: chat.map_or(0, |c| c.epoch),
    }
}

#[component]
pub(super) fn ChatHost(ui: Ui) -> impl IntoView {
    let abort = StoredValue::new(None::<AbortHandle>);
    on_cleanup(move || {
        if let Some(abort) = abort.get_value() {
            abort.abort();
        }
    });
    Effect::new(move |_| {
        let token = ui.token.get();
        if let Some(previous) = abort.get_value() {
            previous.abort();
        }
        ui.unread.set(vec![]);
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
                        let error = connected(ui, &token)
                            .await
                            .err()
                            .unwrap_or_else(|| "Disconnected".into());
                        if let Some(chat) = ui.active_chat.get_untracked() {
                            chat.online.set(vec![]);
                            chat.status.set(format!("{error}. Retrying..."));
                        }
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
}

async fn connected(ui: Ui, token: &str) -> Result<(), String> {
    use std::{cell::Cell, rc::Rc};
    let base = option_env!("THISCORD_API_URL")
        .unwrap_or("http://localhost:3000")
        .trim_end_matches('/');
    let socket = WebSocket::open(&format!("{}{SOCKET_PATH}", base.replacen("http", "ws", 1)))
        .map_err(|_| "Cannot open chat connection")?;
    let (mut sink, mut stream) = socket.split();
    sink.send(Message::Text(
        serde_json::to_string(&ClientFrame {
            version: SOCKET_VERSION,
            event: ClientEvent::Connect {
                token: token.into(),
            },
        })
        .map_err(|_| "Invalid socket request")?,
    ))
    .await
    .map_err(|_| "Connection failed")?;
    let authenticated = Rc::new(Cell::new(false));
    let sent = Rc::new(Cell::new(None::<(u64, Subscription)>));
    let reader = async {
        loop {
            let incoming = futures_util::future::select(
                Box::pin(stream.next()),
                Box::pin(gloo_timers::future::TimeoutFuture::new(25000)),
            )
            .await;
            let text = match incoming {
                futures_util::future::Either::Left((Some(Ok(Message::Text(text))), _)) => text,
                _ => return Err("Connection interrupted".to_string()),
            };
            let frame: ServerFrame =
                serde_json::from_str(&text).map_err(|_| "Invalid socket response")?;
            if frame.version != SOCKET_VERSION {
                return Err("Unsupported socket version".into());
            }
            match frame.event {
                ServerEvent::Authenticated {} => authenticated.set(true),
                ServerEvent::Pong {} => {}
                ServerEvent::Revoked {} => {
                    ui.unread.set(vec![]);
                    if let Some(chat) = ui.active_chat.get_untracked() {
                        let _ = deliver(chat, ServerEvent::Revoked {});
                    }
                    return Err("Access changed".into());
                }
                ServerEvent::Error { error } => return Err(error.message),
                ServerEvent::Subscribed {
                    subscription,
                    history,
                    permissions,
                } => {
                    if !sent
                        .get()
                        .is_some_and(|(id, sub)| id == subscription && sub == desired(ui))
                    {
                        continue;
                    }
                    if let Some(chat) = ui
                        .active_chat
                        .get_untracked()
                        .filter(|c| Some(c.channel) == desired(ui).channel)
                    {
                        chat.permissions.set(permissions);
                        if let Some(history) = history {
                            deliver(chat, ServerEvent::Ready { history })?;
                        }
                    }
                }
                ServerEvent::Update {
                    subscription,
                    event,
                } => {
                    if !sent
                        .get()
                        .is_some_and(|(id, sub)| id == subscription && sub == desired(ui))
                    {
                        continue;
                    }
                    match *event {
                        ServerEvent::Unread { channels } => ui.unread.set(channels),
                        event => {
                            if let Some(chat) = ui
                                .active_chat
                                .get_untracked()
                                .filter(|c| Some(c.channel) == desired(ui).channel)
                            {
                                deliver(chat, event)?;
                            }
                        }
                    }
                }
                _ => return Err("Unexpected socket event".into()),
            }
        }
    };
    let writer = async {
        let mut count = 0_u32;
        let mut serial = 0;
        loop {
            // Local scheduling only: no network request unless a subscription,
            // typing state, read position or heartbeat actually needs sending.
            gloo_timers::future::TimeoutFuture::new(100).await;
            if !authenticated.get() {
                continue;
            }
            count = count.wrapping_add(1);
            let wanted = desired(ui);
            let event = if sent.get().is_none_or(|(_, sub)| sub != wanted) {
                serial += 1;
                sent.set(Some((serial, wanted)));
                ui.unread.set(vec![]);
                Some(ClientEvent::Subscribe {
                    subscription: serial,
                    guild_id: wanted.guild,
                    channel_id: wanted.channel,
                })
            } else if count.is_multiple_of(100) {
                Some(ClientEvent::Ping {})
            } else if count.is_multiple_of(10)
                && ui
                    .active_chat
                    .get_untracked()
                    .is_some_and(|c| Some(c.channel) == wanted.channel && c.typing.get_untracked())
            {
                if let Some(chat) = ui.active_chat.get_untracked() {
                    chat.typing.set(false);
                }
                Some(ClientEvent::Typing { active: true })
            } else {
                None
            };
            if count.is_multiple_of(10)
                && let Some(chat) = ui.active_chat.get_untracked()
            {
                chat.read();
            }
            if let Some(event) = event
                && sink
                    .send(Message::Text(
                        serde_json::to_string(&ClientFrame {
                            version: SOCKET_VERSION,
                            event,
                        })
                        .map_err(|_| "Invalid socket request")?,
                    ))
                    .await
                    .is_err()
            {
                return Err("Connection interrupted".to_string());
            }
        }
    };
    futures_util::try_join!(reader, writer).map(|_: ((), ())| ())
}

fn deliver(chat: Chat, event: ServerEvent) -> Result<(), String> {
    match event {
        ServerEvent::Ready { history } => {
            chat.messages.set(history.messages);
            chat.older.set(history.older);
            let delivered = chat.messages.get_untracked();
            chat.pending
                .update(|p| p.retain(|p| !delivered.iter().any(|m| m.client_id == p.id)));
            chat.status.set("Connected".into());
            chat.scroll();
            chat.read();
        }
        ServerEvent::Message { message, .. } => {
            chat.merge(message);
            chat.read();
        }
        ServerEvent::Presence { members } => chat.online.set(members),
        ServerEvent::Pong {} => {}
        ServerEvent::Revoked {} => {
            chat.messages.set(vec![]);
            chat.online.set(vec![]);
            chat.permissions.set(Permissions::new());
            return Err("Access changed; reconnecting".into());
        }
        ServerEvent::Error { error } => {
            chat.messages.set(vec![]);
            chat.permissions.set(Permissions::new());
            return Err(error.message);
        }
        _ => {}
    }
    Ok(())
}

#[component]
pub(super) fn ChatPanel(ui: Ui, guild: GuildId, channel: ChannelId, name: String) -> impl IntoView {
    ui.chat_epoch.update(|e| *e += 1);
    let epoch = ui.chat_epoch.get_untracked();
    let chat = Chat {
        ui,
        epoch,
        guild,
        channel,
        messages: RwSignal::new(vec![]),
        pending: RwSignal::new(vec![]),
        status: RwSignal::new("Connecting…".into()),
        online: RwSignal::new(vec![]),
        older: RwSignal::new(None),
        permissions: RwSignal::new(Permissions::new()),
        list: NodeRef::new(),
        bottom: RwSignal::new(true),
        typing: RwSignal::new(false),
        last_read: RwSignal::new(0),
    };
    let draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<MessageId>);
    let loading = RwSignal::new(false);
    ui.active_chat.set(Some(chat));
    on_cleanup(move || {
        if chat.alive() {
            ui.active_chat.set(None);
            ui.chat_epoch.update(|e| *e += 1);
        }
    });
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let content = draft.get_untracked();
        if content.trim().is_empty() || content.chars().count() > MAX_MESSAGE_CHARS {
            return;
        }
        if let Some(id) = editing.get_untracked() {
            let Some(old) = chat
                .messages
                .get_untracked()
                .into_iter()
                .find(|m| m.id == id)
            else {
                return;
            };
            loading.set(true);
            leptos::task::spawn_local(async move {
                let result = chat
                    .request(&ChatRequest::Edit {
                        guild_id: guild,
                        channel_id: channel,
                        message_id: id,
                        revision: old.revision,
                        content,
                    })
                    .await;
                if !chat.alive() {
                    return;
                }
                loading.set(false);
                match result {
                    Ok(ChatResponse::Message { message }) => {
                        chat.merge(message);
                        editing.set(None);
                        draft.set(String::new());
                    }
                    Err(e) => chat.status.set(e),
                    _ => {}
                }
            });
        } else {
            let id = window()
                .crypto()
                .ok()
                .and_then(|c| c.random_uuid().parse::<ClientMessageId>().ok());
            if let Some(id) = id {
                chat.send(Pending {
                    id,
                    content,
                    failed: false,
                });
                draft.set(String::new());
            } else {
                chat.status
                    .set("Secure random IDs are unavailable; use HTTPS or localhost".into());
            }
        }
    };
    view! {
        <section class="flex h-full min-h-0 min-w-0 flex-col overflow-hidden rounded-xl border border-white/10 bg-white/5" aria-label="Text chat">
            <header class="shrink-0 border-b border-white/10 p-4"><h2 class="truncate text-lg font-semibold">{format!("# {name}")}</h2><p class="break-words text-xs text-white/60" role="status">{move||chat.status.get()}</p></header>
            <div node_ref=chat.list class="min-h-0 flex-1 overflow-x-hidden overflow-y-auto overscroll-contain p-4" on:scroll=move |_|{if let Some(node)=chat.list.get(){chat.bottom.set(node.scroll_height()-node.scroll_top()-node.client_height()<80);chat.read();}}>
                <Show when=move||chat.older.get().is_some()><button class="mb-4 text-sm text-brand underline" disabled=move||loading.get() on:click=move |_|{
                    loading.set(true);let before=chat.older.get_untracked();let height=chat.list.get().map(|n|n.scroll_height()).unwrap_or(0);let top=chat.list.get().map(|n|n.scroll_top()).unwrap_or(0);chat.bottom.set(false);
                    leptos::task::spawn_local(async move{let result=chat.request(&ChatRequest::History{guild_id:guild,channel_id:channel,before,limit:Default::default()}).await;if !chat.alive(){return;}loading.set(false);match result{Ok(ChatResponse::History{history})=>{chat.older.set(history.older);for message in history.messages{chat.merge(message);}gloo_timers::future::TimeoutFuture::new(0).await;if chat.alive()&&let Some(node)=chat.list.get(){node.set_scroll_top(top+node.scroll_height()-height);}},Err(e)=>chat.status.set(e),_=>{}}});
                }>"Load older messages"</button></Show>
                <ul class="space-y-4">{move||chat.messages.get().into_iter().map(move|m|{
                    let own=ui.account.get_untracked().is_some_and(|a|m.author_id==Some(a.id));let mentioned=ui.account.get_untracked().is_some_and(|a|m.mentions.contains(&a.id));let id=m.id;let revision=m.revision;
                    view!{<li class=if mentioned {"rounded-lg bg-brand/10 px-3 py-2"}else{"rounded-lg px-3 py-2"}><div class="flex flex-wrap items-center gap-3"><strong>{m.username}</strong><time class="text-xs text-white/40">{m.created_at.format("%Y-%m-%d %H:%M").to_string()}</time><Show when=move||m.edited_at.is_some()&&!m.deleted><span class="text-xs text-white/40">"edited"</span></Show></div>
                        <p class="whitespace-pre-wrap break-words text-white/85">{if m.deleted{"Message deleted".into()}else{m.content}}</p>
                        <Show when=move||!m.deleted><div class="flex gap-3 text-xs text-white/50">
                            <Show when=move||own&&chat.permissions.get().contains(&Permission::EditOwnMessages)><button class="hover:underline" on:click=move |_|{editing.set(Some(id));draft.set(chat.messages.get_untracked().into_iter().find(|m|m.id==id).map(|m|m.content).unwrap_or_default());}>"Edit"</button></Show>
                            <Show when=move||chat.permissions.get().contains(&Permission::ManageMessages)||(own&&chat.permissions.get().contains(&Permission::DeleteOwnMessages))><button class="hover:underline" on:click=move |_|{
                                if !window().confirm_with_message("Delete this message?").unwrap_or(false){return;}
                                leptos::task::spawn_local(async move{let result=chat.request(&ChatRequest::Delete{guild_id:guild,channel_id:channel,message_id:id,revision}).await;if !chat.alive(){return;}match result{Ok(ChatResponse::Message{message})=>chat.merge(message),Err(e)=>chat.status.set(e),_=>{}}});
                            }>"Delete"</button></Show>
                        </div></Show>
                    </li>}
                }).collect_view()}</ul>
                <ul class="mt-4 space-y-3">{move||chat.pending.get().into_iter().map(move|p|{let retry_id=p.id;view!{<li class="whitespace-pre-wrap break-words text-white/50"><p>{p.content}</p>{if p.failed{"Failed to send"}else{"Sending…"}}<Show when=move||p.failed><button class="ml-3 text-brand underline" on:click=move |_|if let Some(p)=chat.pending.get_untracked().into_iter().find(|p|p.id==retry_id){chat.send(p);}>"Retry"</button></Show></li>}}).collect_view()}</ul>
            </div>
            <Show when=move||!chat.bottom.get()><button class="bg-brand/20 py-2 text-sm" on:click=move |_|{chat.bottom.set(true);chat.scroll();chat.read();}>"Jump to latest / mark read"</button></Show>
            <footer class="shrink-0 space-y-2 border-t border-white/10 p-3">
                <p class="text-xs text-white/50">{move||format!("Online here: {}",chat.online.get().iter().map(|m|m.username.clone()).collect::<Vec<_>>().join(", "))}</p>
                <p class="min-h-4 text-xs text-white/60" aria-live="polite">{move||{let names=chat.online.get().into_iter().filter(|m|m.typing&&ui.account.get().is_none_or(|a|a.id!=m.account_id)).map(|m|m.username).collect::<Vec<_>>();if names.is_empty(){String::new()}else{format!("{} typing…",names.join(", "))}}}</p>
                <Show when=move||editing.get().is_some()><button class="text-sm text-brand underline" on:click=move |_|{editing.set(None);draft.set(String::new());}>"Cancel edit"</button></Show>
                <form class="flex gap-3" on:submit=submit><label class="min-w-0 flex-1"><span class="sr-only">"Message"</span><textarea class="w-full resize-none rounded-lg bg-black/20 p-3 outline-none focus:ring-2 focus:ring-brand" rows="2" maxlength="4000" placeholder="Message this channel · @username to mention" prop:value=move||draft.get() disabled=move||!chat.permissions.get().contains(&Permission::SendMessages) on:input=move|ev|{draft.set(event_target_value(&ev));chat.typing.set(true);}/></label><button class="self-end rounded-lg bg-brand px-4 py-3 disabled:opacity-50" disabled=move||loading.get()||draft.get().trim().is_empty()||!chat.permissions.get().contains(&Permission::SendMessages)>{move||if editing.get().is_some(){"Save"}else{"Send"}}</button></form>
            </footer>
        </section>
    }
}
