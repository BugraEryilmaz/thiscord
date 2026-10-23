use super::Ui;
use super::icons::{Glyph, Icon};
use futures_util::{
    SinkExt, StreamExt,
    future::{AbortHandle, Abortable},
};
use gloo_net::websocket::{Message, futures::WebSocket};
use leptos::prelude::*;
use std::collections::HashSet;
use thiscord_frontend::chat_history::{Anchor, Merge, MessageCache};
use thiscord_frontend::rich_chat::{RichChat, SnapshotStatus};
use thiscord_shared::{
    ChannelId, ClientMessageId, GuildId, MessageId,
    chat::*,
    permissions::{Permission, Permissions},
};
use wasm_bindgen::{JsCast, closure::Closure};

#[path = "attachments.rs"]
mod attachments;
#[path = "mentions.rs"]
mod mentions;
#[path = "rich_chat.rs"]
mod rich;
pub(super) use attachments::{Permit as AttachmentPermit, transfer as attachment_transfer};
use thiscord_frontend::attachments::{PendingSend as Pending, can_send};

const PENDING_LIMIT: usize = 20;

#[derive(Clone, Copy)]
pub(super) struct Chat {
    ui: Ui,
    epoch: u64,
    attachment_generation: RwSignal<u64>,
    access_generation: RwSignal<u64>,
    guild: GuildId,
    channel: ChannelId,
    messages: RwSignal<MessageCache<ArcRwSignal<ChatMessage>>>,
    pending: RwSignal<Vec<Pending>>,
    status: RwSignal<String>,
    online: RwSignal<Vec<OnlineMember>>,
    permissions: RwSignal<Permissions>,
    list: NodeRef<leptos::html::Div>,
    rows: NodeRef<leptos::html::Ul>,
    viewport: RwSignal<(f64, f64)>,
    layout_pending: RwSignal<bool>,
    programmatic_top: RwSignal<Option<i32>>,
    anchor: RwSignal<Option<Anchor>>,
    refresh: RwSignal<u64>,
    history_generation: RwSignal<u64>,
    history_loading: RwSignal<bool>,
    ready: RwSignal<bool>,
    bottom: RwSignal<bool>,
    typing: RwSignal<bool>,
    last_read: RwSignal<i64>,
    reply_to: RwSignal<Option<MessageId>>,
    rich: RwSignal<RichChat>,
    highlight: RwSignal<Option<MessageId>>,
}
impl Chat {
    fn alive(self) -> bool {
        self.ui.chat_epoch.try_get_untracked() == Some(self.epoch)
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
        if self.layout_pending.get_untracked() {
            return;
        }
        self.layout_pending.set(true);
        if self.bottom.get_untracked() {
            self.anchor.set(None);
        } else if self.anchor.get_untracked().is_none() {
            // Retain the same message across subsequent measurement passes.
            // Recapturing after a prepend can anchor a newly inserted row and
            // shift the original message when that row's estimate is corrected.
            self.anchor.set(
                self.messages
                    .with_untracked(|messages| messages.anchor(self.viewport.get_untracked().0)),
            );
        }
        leptos::task::spawn_local(async move {
            // Coalesce bursts and ResizeObserver deliveries at frame cadence.
            // A timer also settles state in a hidden/minimized native WebView.
            gloo_timers::future::TimeoutFuture::new(16).await;
            if !self.alive() {
                return;
            }
            self.layout_pending.set(false);
            if let Some(node) = self.list.get_untracked() {
                if self.bottom.get_untracked() {
                    node.set_scroll_top(node.scroll_height());
                } else if let Some(anchor) = self.anchor.get_untracked() {
                    let top = self.messages.with_untracked(|m| m.anchor_top(anchor));
                    if top.is_none() {
                        self.anchor.set(None);
                    }
                    // WebView geometry can be fractional at non-default scale.
                    // Truncation would accumulate a pixel of drift on every pass.
                    node.set_scroll_top((top.unwrap_or(0.0) + self.rows_offset()).round() as i32);
                }
                self.programmatic_top.set(Some(node.scroll_top()));
                self.update_viewport();
            }
        });
    }
    fn rows_offset(self) -> f64 {
        match (self.list.get_untracked(), self.rows.get_untracked()) {
            (Some(list), Some(rows)) => {
                rows.get_bounding_client_rect().top() - list.get_bounding_client_rect().top()
                    + f64::from(list.scroll_top())
            }
            _ => 0.0,
        }
    }
    fn update_viewport(self) {
        let _timing = crate::performance::start(2);
        if let Some(node) = self.list.get_untracked() {
            let viewport = (
                f64::from(node.scroll_top()) - self.rows_offset(),
                f64::from(node.client_height()),
            );
            if self.viewport.get_untracked() != viewport {
                self.viewport.set(viewport);
            }
        }
    }
    fn merge_batch(self, messages: Vec<ChatMessage>, mode: Merge) {
        self.apply_batch(messages, mode, false);
    }
    fn apply_batch(self, messages: Vec<ChatMessage>, mode: Merge, replace: bool) {
        let _timing = crate::performance::start(1);
        let messages = self
            .rich
            .try_update(|rich| {
                rich.reconcile(messages, |id| {
                    self.messages
                        .with_untracked(|cache| cache.get(id).map(|m| m.get_untracked()))
                })
            })
            .expect("live chat state");
        // Capture the visual anchor before changing heights/order. All work in a
        // history page shares one cache mutation and one deferred scroll pass.
        self.scroll();
        for message in &messages {
            if message.deleted {
                if self.reply_to.get_untracked() == Some(message.id) {
                    self.reply_to.set(None);
                }
                attachments::revoke_message(self, message.id);
            }
        }
        let delivered: HashSet<_> = messages.iter().map(|m| m.client_id).collect();
        self.pending
            .update(|p| p.retain(|p| !delivered.contains(&p.id)));
        self.messages.update(|cache| {
            if replace {
                *cache = MessageCache::default();
                // A context window must never acknowledge the live edge as read.
                cache.newer = true;
            }
            cache.merge(messages.clone(), mode, ArcRwSignal::new, |row, message| {
                row.set(message)
            });
            for row in cache.values() {
                let update = row.with_untracked(|current| {
                    current.reply.as_ref().and_then(|reply| {
                        messages
                            .iter()
                            .find(|m| m.id == reply.id)
                            .map(thiscord_frontend::rich_chat::preview)
                    })
                });
                if let Some(preview) = update {
                    row.update(|current| {
                        if let Some(reply) = &mut current.reply {
                            reply.preview = preview;
                        }
                    });
                }
            }
        });
    }
    fn merge(self, message: ChatMessage) {
        self.merge_batch(
            vec![message],
            Merge::Live {
                // Do not evict the boundary of an in-flight older page.
                follow: self.bottom.get_untracked() && !self.history_loading.get_untracked(),
            },
        );
    }
    fn reset(self) {
        self.rich.update(RichChat::reset);
        self.reply_to.set(None);
        self.highlight.set(None);
        attachments::clear_downloads(self);
        self.attachment_generation.update(|g| *g += 1);
        // An interrupted request may have committed; retries keep its exact IDs.
        self.pending
            .update(|items| items.iter_mut().for_each(|item| item.failed = true));
        self.history_generation
            .update(|generation| *generation += 1);
        self.history_loading.set(false);
        self.ready.set(false);
        self.messages.set(MessageCache::default());
        self.anchor.set(None);
    }
    fn latest(self) {
        if self.history_loading.get_untracked() {
            self.history_generation
                .update(|generation| *generation += 1);
            self.history_loading.set(false);
            self.rich.update(RichChat::cancel_context);
        }
        self.bottom.set(true);
        if self.messages.with_untracked(|m| m.newer) {
            // Re-subscribe for an atomic history/event snapshot, avoiding a gap
            // between an HTTP latest page and concurrent socket updates.
            self.reset();
            self.refresh.update(|generation| *generation += 1);
            self.status.set("Loading latest messages…".into());
        }
        self.scroll();
        self.read();
    }
    fn load_older(self) {
        if self.history_loading.get_untracked() || !self.ready.get_untracked() {
            return;
        }
        let Some(before) = self.messages.with_untracked(|m| m.older.clone()) else {
            return;
        };
        let generation = self.history_generation.get_untracked();
        self.history_loading.set(true);
        self.bottom.set(false);
        leptos::task::spawn_local(async move {
            let result = self
                .request(&ChatRequest::History {
                    guild_id: self.guild,
                    channel_id: self.channel,
                    before: Some(before),
                    limit: Default::default(),
                })
                .await;
            if !self.alive() || self.history_generation.get_untracked() != generation {
                return;
            }
            self.history_loading.set(false);
            match result {
                Ok(ChatResponse::History { history }) => {
                    self.bottom.set(false);
                    self.messages.update_untracked(|m| m.older = history.older);
                    self.merge_batch(history.messages, Merge::Older);
                }
                Err(error) => self.status.set(error),
                _ => {}
            }
        });
    }
    fn read(self) {
        if !self.ready.get_untracked()
            || !self.bottom.get_untracked()
            || self.messages.with_untracked(|m| m.newer)
            || !document().has_focus().unwrap_or(false)
        {
            return;
        }
        let through = self.messages.with_untracked(|m| m.latest_sequence());
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
        if !self
            .permissions
            .with_untracked(|p| p.contains(&Permission::SendMessages))
        {
            return;
        }
        if self
            .pending
            .with_untracked(|p| p.len() >= PENDING_LIMIT && !p.iter().any(|p| p.id == pending.id))
        {
            self.status
                .set("Resolve or discard a pending message before sending more".into());
            return;
        }
        self.pending.update(|items| {
            if let Some(p) = items.iter_mut().find(|p| p.id == pending.id) {
                p.failed = false;
            } else {
                items.push(pending.clone());
            }
        });
        self.latest();
        let generation = self.access_generation.get_untracked();
        leptos::task::spawn_local(async move {
            let response = self
                .request(&pending.request(self.guild, self.channel))
                .await;
            if !self.alive() || self.access_generation.get_untracked() != generation {
                return;
            }
            match response {
                Ok(ChatResponse::Message { message }) => {
                    self.status.set("Message sent".into());
                    self.merge(*message);
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
pub(super) enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting,
    Offline,
}
impl ConnectionStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Connecting => crate::i18n::t("Connecting…"),
            Self::Connected => crate::i18n::t("Connected"),
            Self::Reconnecting => crate::i18n::t("Reconnecting…"),
            Self::Offline => crate::i18n::t("Offline"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Subscription {
    guild: Option<GuildId>,
    channel: Option<ChannelId>,
    epoch: u64,
    refresh: u64,
}
fn desired(ui: Ui) -> Subscription {
    let guild = ui.server.with_untracked(|s| s.as_ref().map(|s| s.guild.id));
    let chat = ui
        .active_chat
        .get_untracked()
        .filter(|c| c.alive() && Some(c.guild) == guild);
    Subscription {
        guild,
        channel: chat.map(|c| c.channel),
        epoch: chat.map_or(0, |c| c.epoch),
        refresh: chat.map_or(0, |c| c.refresh.get_untracked()),
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
        ui.voice_roster.set(vec![]);
        ui.socket_status.set(ConnectionStatus::Offline);
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
                        if !window().navigator().on_line() {
                            ui.socket_status.set(ConnectionStatus::Offline);
                            gloo_timers::future::TimeoutFuture::new(1000).await;
                            continue;
                        }
                        ui.socket_status.set(ConnectionStatus::Connecting);
                        let started = js_sys::Date::now();
                        let error = connected(ui, &token)
                            .await
                            .err()
                            .unwrap_or_else(|| "Disconnected".into());
                        ui.socket_status.set(if window().navigator().on_line() {
                            ConnectionStatus::Reconnecting
                        } else {
                            ConnectionStatus::Offline
                        });
                        ui.voice_roster.set(vec![]);
                        ui.unread.set(vec![]);
                        if let Some(chat) = ui.active_chat.get_untracked() {
                            chat.online.set(vec![]);
                            chat.status.set(thiscord_frontend::localized_format!(
                                crate::i18n::language(),
                                "{error}. Retrying...",
                                error = crate::i18n::message(error)
                            ));
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
        .map_err(|_| crate::i18n::t("Cannot open chat connection"))?;
    let (mut sink, mut stream) = socket.split();
    sink.send(Message::Text(
        serde_json::to_string(&ClientFrame {
            version: SOCKET_VERSION,
            event: ClientEvent::Connect {
                token: token.into(),
            },
        })
        .map_err(|_| crate::i18n::t("Invalid socket request"))?,
    ))
    .await
    .map_err(|_| crate::i18n::t("Connection failed"))?;
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
            let frame: ServerFrame = serde_json::from_str(&text)
                .map_err(|_| crate::i18n::t("Invalid socket response"))?;
            if frame.version != SOCKET_VERSION {
                return Err("Unsupported socket version".into());
            }
            match frame.event {
                ServerEvent::Authenticated {} => {
                    authenticated.set(true);
                    ui.socket_status.set(ConnectionStatus::Connected);
                }
                ServerEvent::Pong {} => {}
                ServerEvent::Revoked {} => {
                    ui.unread.set(vec![]);
                    if let Some(chat) = ui.active_chat.get_untracked() {
                        let _ = deliver(chat, ServerEvent::Revoked {});
                    }
                    return Err("Access changed".into());
                }
                ServerEvent::Error { error } => {
                    if error.code == thiscord_shared::ErrorCode::Unauthorized
                        && crate::account_client::session_rejected(token).await
                    {
                        // Clearing the token aborts this socket task. Finish native
                        // credential/voice cleanup on an independent task, and never
                        // clear a replacement login because of an old socket reply.
                        let token = token.to_owned();
                        leptos::task::spawn_local(async move {
                            if ui.token.get_untracked().as_ref() == Some(&token) {
                                super::clear_local(ui).await;
                                if ui.token.get_untracked().is_none() {
                                    ui.status.set("Please sign in again.".into());
                                }
                            }
                        });
                    }
                    // Authoritative socket errors (including a session revoked
                    // while offline) must invalidate assets before reconnecting.
                    if let Some(chat) = ui.active_chat.get_untracked() {
                        return deliver(chat, ServerEvent::Error { error });
                    }
                    return Err(crate::i18n::api_error(&error));
                }
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
                        if chat.permissions.with_untracked(|p| {
                            p.contains(&Permission::SendMessages)
                                && !permissions.contains(&Permission::SendMessages)
                        }) {
                            // Reconnect can reveal permission changes without a
                            // Revoked event having reached this client.
                            chat.access_generation.update(|g| *g += 1);
                            chat.pending.set(vec![]);
                        }
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
                        ServerEvent::VoiceRoster { channels } => ui.voice_roster.set(channels),
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
            if !window().navigator().on_line() {
                return Err("Offline".to_string());
            }
            if !authenticated.get() {
                continue;
            }
            count = count.wrapping_add(1);
            let wanted = desired(ui);
            let event = if sent.get().is_none_or(|(_, sub)| sub != wanted) {
                serial += 1;
                sent.set(Some((serial, wanted)));
                ui.unread.set(vec![]);
                ui.voice_roster.set(vec![]);
                Some(ClientEvent::Subscribe {
                    subscription: serial,
                    guild_id: wanted.guild,
                    channel_id: wanted.channel,
                    voice_roster: true,
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
                        .map_err(|_| crate::i18n::t("Invalid socket request"))?,
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
            chat.reset();
            chat.bottom.set(true);
            chat.messages.update_untracked(|m| m.older = history.older);
            chat.merge_batch(history.messages, Merge::Older);
            chat.ready.set(true);
            chat.status.set("Connected".into());
            chat.scroll();
            chat.read();
        }
        ServerEvent::Message { message, .. } => {
            chat.merge(*message);
            chat.read();
        }
        ServerEvent::Presence { members } => chat.online.set(members),
        ServerEvent::Pong {} => {}
        ServerEvent::Revoked {} => {
            chat.access_generation.update(|g| *g += 1);
            chat.pending.set(vec![]);
            chat.reset();
            chat.online.set(vec![]);
            chat.permissions.set(Permissions::new());
            return Err("Access changed; reconnecting".into());
        }
        ServerEvent::Error { error } => {
            chat.access_generation.update(|g| *g += 1);
            chat.pending.set(vec![]);
            chat.reset();
            chat.permissions.set(Permissions::new());
            return Err(crate::i18n::api_error(&error));
        }
        _ => {}
    }
    Ok(())
}

/// Observers and callbacks are disposed with the keyed row/effect that owns them.
fn observe_size(element: &web_sys::Element, callback: impl FnMut() + 'static) {
    let callback = Closure::<dyn FnMut()>::new(callback);
    if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
        observer.observe(element);
        let resource = StoredValue::new_local((observer, callback));
        on_cleanup(move || resource.with_value(|(observer, _)| observer.disconnect()));
    }
}

#[component]
fn MessageRow(
    chat: Chat,
    id: MessageId,
    message: ArcRwSignal<ChatMessage>,
    editing: RwSignal<Option<MessageId>>,
    draft: RwSignal<String>,
) -> impl IntoView {
    // This arena handle belongs to the keyed row, not the chat panel. The cache
    // retains only the Arc; evicted rows release both state and measurements.
    let message = RwSignal::from(message);
    let row = NodeRef::<leptos::html::Li>::new();
    let own = move || {
        message.with(|m| {
            chat.ui
                .account
                .with(|a| a.as_ref().is_some_and(|a| m.author_id == Some(a.id)))
        })
    };
    Effect::new(move |_| {
        if let Some(node) = row.get() {
            observe_size(node.unchecked_ref(), move || {
                if !chat.alive() {
                    return;
                }
                let Some(node) = row.get_untracked() else {
                    return;
                };
                chat.scroll();
                let _timing = crate::performance::start(2);
                let changed = chat
                    .messages
                    .update_untracked(|m| m.measure(id, node.get_bounding_client_rect().height()));
                if changed {
                    chat.messages.notify();
                }
            });
        }
    });
    view! {
        <li node_ref=row class="min-h-20 pb-4" class:ring-2=move || chat.highlight.get()==Some(id) class:ring-brand=move || chat.highlight.get()==Some(id) data-message-id=id.to_string()>
            <div class=move || if message.with(|m| chat.ui.account.with(|a| a.as_ref().is_some_and(|a| m.mentions.contains(&a.id)))) {
                "rounded-lg bg-brand/10 px-3 py-2"
            } else { "rounded-lg px-3 py-2" }>
                <div class="flex flex-wrap items-center gap-3">
                    <button class="font-semibold hover:underline" aria-haspopup="menu" data-context-user=move ||message.with(|m|m.author_id.map(|id|id.to_string())) data-context-name=move ||message.with(|m|m.display_name().to_owned())>{move || message.with(|m| m.display_name().to_owned())}</button>
                    <time class="text-xs text-white/40">{move || message.with(|m| crate::i18n::datetime(m.created_at))}</time>
                    <Show when=move || message.with(|m| m.edited_at.is_some() && !m.deleted)><span class="text-xs text-white/40">{move ||crate::i18n::t("edited")}</span></Show>
                </div>
                <rich::Reply chat message/>
                <div class="whitespace-pre-wrap break-words text-white/85">{move || message.with(|m| super::message_content::render(if m.deleted { "Message deleted" } else { &m.content }, chat.ui))}</div>
                <Show when=move || message.with(|m| !m.deleted)>
                    <For each=move || message.with(|m| m.attachments.clone()) key=|a| a.id children=move |attachment| view! { <attachments::Card chat message=id attachment/> }/>
                    <rich::Actions chat message/>
                    <div class="flex gap-3 text-xs text-white/50">
                        <Show when=move ||!own()><super::safety::ReportButton ui=chat.ui target=thiscord_shared::safety::ReportTarget::GuildMessage {guild_id:chat.guild,channel_id:chat.channel,message_id:id}/></Show>
                        <Show when=move || own() && chat.permissions.with(|p| p.contains(&Permission::EditOwnMessages))>
                            <button class="icon-button" aria-label=move || crate::i18n::t("Edit message") data-tip=move || crate::i18n::t("Edit") on:click=move |_| {
                                editing.set(Some(id));
                                draft.set(message.with_untracked(|m| m.content.clone()));
                            }><Icon kind=Glyph::Edit/></button>
                        </Show>
                        <Show when=move || chat.permissions.with(|p| p.contains(&Permission::ManageMessages) || (own() && p.contains(&Permission::DeleteOwnMessages)))>
                            <button class="icon-button danger" aria-label=move || crate::i18n::t("Delete message") data-tip=move || crate::i18n::t("Delete") on:click=move |_| {
                                if !window().confirm_with_message(crate::i18n::t("Delete this message?")).unwrap_or(false) { return; }
                                let revision = message.with_untracked(|m| m.revision);
                                let generation = chat.access_generation.get_untracked();
                                leptos::task::spawn_local(async move {
                                    let result = chat.request(&ChatRequest::Delete {
                                        guild_id: chat.guild, channel_id: chat.channel, message_id: id, revision,
                                    }).await;
                                    if !chat.alive() || chat.access_generation.get_untracked() != generation { return; }
                                    match result {
                                        Ok(ChatResponse::Message { message }) => chat.merge(*message),
                                        Err(error) => chat.status.set(error),
                                        _ => {}
                                    }
                                });
                            }><Icon kind=Glyph::Trash/></button>
                        </Show>
                    </div>
                </Show>
            </div>
        </li>
    }
}

#[component]
pub(super) fn ChatPanel(ui: Ui, guild: GuildId, channel: ChannelId, name: String) -> impl IntoView {
    ui.chat_epoch.update(|e| *e += 1);
    let epoch = ui.chat_epoch.get_untracked();
    let chat = Chat {
        ui,
        epoch,
        attachment_generation: RwSignal::new(0),
        access_generation: RwSignal::new(0),
        guild,
        channel,
        messages: RwSignal::new(MessageCache::default()),
        pending: RwSignal::new(vec![]),
        status: RwSignal::new("Connecting…".into()),
        online: RwSignal::new(vec![]),
        permissions: RwSignal::new(Permissions::new()),
        list: NodeRef::new(),
        rows: NodeRef::new(),
        viewport: RwSignal::new((0.0, 800.0)),
        layout_pending: RwSignal::new(false),
        programmatic_top: RwSignal::new(None),
        anchor: RwSignal::new(None),
        refresh: RwSignal::new(0),
        history_generation: RwSignal::new(0),
        history_loading: RwSignal::new(false),
        ready: RwSignal::new(false),
        bottom: RwSignal::new(true),
        typing: RwSignal::new(false),
        last_read: RwSignal::new(0),
        reply_to: RwSignal::new(None),
        rich: RwSignal::new(RichChat::default()),
        highlight: RwSignal::new(None),
    };
    let draft = RwSignal::new(String::new());
    let mentions = mentions::Picker::new(chat, draft);
    let editing = RwSignal::new(None::<MessageId>);
    let loading = RwSignal::new(false);
    let queue = attachments::Queue::new(chat);
    Effect::new(move |_| {
        ui.token.track();
        chat.access_generation.update(|g| *g += 1);
        chat.rich.update(RichChat::reset);
        attachments::clear_downloads(chat);
        chat.attachment_generation.update(|g| *g += 1);
    });
    let visible = Memo::new(move |_| {
        let (top, height) = chat.viewport.get();
        chat.messages.with(|m| m.window(top, height))
    });
    Effect::new(move |_| {
        if let Some(node) = chat.list.get() {
            observe_size(node.unchecked_ref(), move || {
                if chat.alive() {
                    chat.scroll();
                    chat.update_viewport();
                }
            });
            chat.update_viewport();
        }
    });
    ui.active_chat.set(Some(chat));
    Effect::new(move |_| {
        if chat.ready.get()
            && let Some((channel_id, message_id)) = ui.chat_jump.get()
            && channel_id == channel
        {
            ui.chat_jump.set(None);
            chat.jump(message_id);
        }
    });
    on_cleanup(move || {
        if chat.alive() {
            ui.active_chat.set(None);
            ui.chat_epoch.update(|e| *e += 1);
        }
    });
    let composer_ready = move || {
        if let Some(id) = editing.get() {
            let count = chat.messages.with(|m| {
                m.get(id)
                    .map(|m| m.with(|m| m.attachments.len()))
                    .unwrap_or(0)
            });
            draft.with(|content| can_send(content, count))
        } else {
            queue.ready() && draft.with(|content| can_send(content, queue.count()))
        }
    };
    let send = move || {
        if loading.get_untracked()
            || !chat
                .permissions
                .with_untracked(|p| p.contains(&Permission::SendMessages))
        {
            return;
        }
        let content = draft.get_untracked();
        if !composer_ready() || content.chars().count() > MAX_MESSAGE_CHARS {
            return;
        }
        if let Some(id) = editing.get_untracked() {
            let Some(revision) = chat
                .messages
                .with_untracked(|m| m.get(id).map(|m| m.with_untracked(|m| m.revision)))
            else {
                chat.status
                    .set("That message left the history window; load it again to edit".into());
                return;
            };
            loading.set(true);
            let generation = chat.access_generation.get_untracked();
            leptos::task::spawn_local(async move {
                let result = chat
                    .request(&ChatRequest::Edit {
                        guild_id: guild,
                        channel_id: channel,
                        message_id: id,
                        revision,
                        content,
                    })
                    .await;
                if !chat.alive() {
                    return;
                }
                loading.set(false);
                if chat.access_generation.get_untracked() != generation {
                    return;
                }
                match result {
                    Ok(ChatResponse::Message { message }) => {
                        chat.merge(*message);
                        editing.set(None);
                        draft.set(String::new());
                    }
                    Err(e) => chat.status.set(e),
                    _ => {}
                }
            });
        } else {
            if chat.pending.with_untracked(|p| p.len() >= PENDING_LIMIT) {
                chat.status
                    .set("Resolve or discard a pending message before sending more".into());
                return;
            }
            let id = window()
                .crypto()
                .ok()
                .and_then(|c| c.random_uuid().parse::<ClientMessageId>().ok());
            if let Some(id) = id {
                let attachments = queue.take();
                chat.send(Pending {
                    attachment_ids: attachments.iter().map(|a| a.id).collect(),
                    filenames: attachments.into_iter().map(|a| a.filename).collect(),
                    id,
                    content,
                    failed: false,
                    reply_to: chat.reply_to.get_untracked(),
                });
                draft.set(String::new());
                chat.reply_to.set(None);
            } else {
                chat.status
                    .set("Secure random IDs are unavailable; use HTTPS or localhost".into());
            }
        }
    };
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        send();
    };
    let keydown = move |ev: leptos::ev::KeyboardEvent| {
        if mentions.keydown(&ev) {
            return;
        }
        // Leave Enter available for IME candidate selection and Shift+Enter newlines.
        if ev.key() == "Enter" && !ev.shift_key() && !ev.is_composing() && ev.key_code() != 229 {
            ev.prevent_default();
            if !ev.repeat() {
                send();
            }
        }
    };
    view! {
        <section class="flex h-full min-h-0 min-w-0 flex-col overflow-hidden rounded-xl border border-white/10 bg-white/5" aria-label=move || crate::i18n::t("Text chat")
            on:dragover=move |ev: leptos::ev::DragEvent| { ev.prevent_default(); if let Some(data) = ev.data_transfer() { data.set_drop_effect(if editing.get_untracked().is_some() { "none" } else { "copy" }); } }
            on:drop=move |ev: leptos::ev::DragEvent| { ev.prevent_default(); if editing.get_untracked().is_none() && let Some(files) = ev.data_transfer().and_then(|d| d.files()) { queue.add(files); } }>
            <attachments::DownloadHost chat/>
            <header class="shrink-0 border-b border-white/10 p-4"><h2 class="truncate text-lg font-semibold">{format!("# {name}")}</h2><rich::Pins chat/><super::search::Launcher ui scope=thiscord_shared::search::Scope::Guild { guild_id: guild, channel_id: None }/><p class="break-words text-xs text-white/60" role="status">{move||crate::i18n::message(chat.status.get())}</p></header>
            <div node_ref=chat.list class="min-h-0 flex-1 overflow-x-hidden overflow-y-auto overscroll-contain p-4" style="overflow-anchor: none" on:scroll=move |_| {
                chat.update_viewport();
                if !chat.layout_pending.get_untracked() && let Some(node) = chat.list.get_untracked() {
                    if chat.programmatic_top.get_untracked() == Some(node.scroll_top()) { return; }
                    chat.programmatic_top.set(None);
                    chat.anchor.set(None);
                    chat.bottom.set(node.scroll_height() - node.scroll_top() - node.client_height() < 80);
                    chat.read();
                }
            }>
                <div class="h-10">
                    <Show when=move || chat.messages.with(|m| m.older.is_some())>
                        <button class="text-sm text-brand underline" disabled=move || chat.history_loading.get()
                            on:click=move |_| chat.load_older()>{move || crate::i18n::t("Load older messages")}</button>
                    </Show>
                </div>
                <ul node_ref=chat.rows>
                    <li aria-hidden="true" style:height=move || visible.with(|w| format!("{}px", w.before))></li>
                    <For
                        each=move || {
                            let generation = chat.history_generation.get();
                            visible.with(|w| chat.messages.with_untracked(|cache| w.ids.iter()
                                .filter_map(|id| cache.get(*id).map(|row| (generation, *id, row.clone()))).collect::<Vec<_>>()))
                        }
                        key=|(generation, id, _)| (*generation, *id)
                        children=move |(_, id, message)| view! { <MessageRow chat id message editing draft/> }
                    />
                    <li aria-hidden="true" style:height=move || visible.with(|w| format!("{}px", w.after))></li>
                </ul>
                <ul class="mt-4 space-y-3">
                    <For each=move || chat.pending.with(|p| p.iter().map(|p| p.id).collect::<Vec<_>>()) key=|id| *id
                        children=move |id| {
                            let pending = Memo::new(move |_| chat.pending.with(|p| p.iter().find(|p| p.id == id).cloned()));
                            view! {
                                <li class="whitespace-pre-wrap break-words text-white/50">
                                    <div>{move || pending.with(|p| super::message_content::render(p.as_ref().map(|p| p.content.as_str()).unwrap_or_default(), chat.ui))}</div>
                                    <p class="text-xs">{move || pending.with(|p| p.as_ref().map(|p| p.filenames.join(", ")).unwrap_or_default())}</p>
                                    {move || if pending.with(|p| p.as_ref().is_some_and(|p| p.failed)) { crate::i18n::t("Failed to send") } else { crate::i18n::t("Sending…") }}
                                    <Show when=move || pending.with(|p| p.as_ref().is_some_and(|p| p.failed))>
                                        <button class="icon-button" aria-label=move || crate::i18n::t("Retry message") data-tip=move || crate::i18n::t("Retry") on:click=move |_| {
                                            if let Some(p) = chat.pending.with_untracked(|p| p.iter().find(|p| p.id == id).cloned()) { chat.send(p); }
                                        }><Icon kind=Glyph::Refresh/></button>
                                        <button class="icon-button" aria-label=move || crate::i18n::t("Discard message") data-tip=move || crate::i18n::t("Discard") on:click=move |_| chat.pending.update(|p| p.retain(|p| p.id != id))><Icon kind=Glyph::Trash/></button>
                                    </Show>
                                </li>
                            }
                        }
                    />
                </ul>
            </div>
            <Show when=move || !chat.bottom.get() || chat.messages.with(|m| m.newer)>
                <button class="bg-brand/20 py-2 text-sm" on:click=move |_| chat.latest()>{move || crate::i18n::t("Jump to latest / mark read")}</button>
            </Show>
            <footer class="shrink-0 space-y-2 border-t border-white/10 p-3">
                <p class="text-xs text-white/50">{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Online here: {}",chat.online.with(|members| members.iter().map(|m|m.display_name()).collect::<Vec<_>>().join(", ")))}</p>
                <p class="min-h-4 text-xs text-white/60" aria-live="polite">{move||{let names=chat.online.with(|members| members.iter().filter(|m|m.typing&&ui.account.with(|a|a.as_ref().is_none_or(|a|a.id!=m.account_id))).map(|m|m.display_name().to_owned()).collect::<Vec<_>>());if names.is_empty(){String::new()}else{thiscord_frontend::localized_format!(crate::i18n::language(), "{} typing…",names.join(", "))}}}</p>
                <Show when=move||editing.get().is_some()><button class="text-sm text-brand underline" on:click=move |_|{editing.set(None);draft.set(String::new());}>{move || crate::i18n::t("Cancel edit")}</button></Show>
                <Show when=move ||chat.reply_to.get().is_some()><div class="flex gap-3 text-sm text-sky-300"><span class="max-w-80 truncate">{move || chat.reply_to.get().map(|id|chat.messages.with(|cache|cache.get(id).map(|m|m.with(|m|thiscord_frontend::localized_format!(crate::i18n::language(), "Reply to {}: {}",m.display_name(),m.content.chars().take(80).collect::<String>()))).unwrap_or_else(||crate::i18n::t("Reply selected").into())))}</span><button on:click=move |_|chat.reply_to.set(None)>{move ||crate::i18n::t("Cancel reply")}</button></div></Show>
                <attachments::Composer queue editing/>
                <form class="flex gap-3" on:submit=submit>
                    <div class="relative min-w-0 flex-1">
                        <mentions::Suggestions picker=mentions/>
                        <label><span class="sr-only">{move || crate::i18n::t("Message")}</span>
                            <textarea node_ref=mentions.input class="w-full resize-none rounded-lg bg-black/20 p-3 outline-none focus:ring-2 focus:ring-brand"
                                aria-autocomplete="list" aria-controls="mention-suggestions" aria-expanded=move || mentions.expanded().to_string()
                                aria-activedescendant=move || mentions.active_descendant()
                                on:keydown=keydown on:click=move |_| mentions.refresh() on:select=move |_| mentions.refresh()
                                on:keyup=move |ev| if matches!(ev.key().as_str(), "ArrowLeft" | "ArrowRight" | "Home" | "End") { mentions.refresh(); }
                                on:blur=move |_| mentions.dismiss()
                                on:compositionstart=move |_| mentions.composition(true) on:compositionend=move |_| mentions.composition(false)
                                rows="2" maxlength="4000" placeholder=move || crate::i18n::t("Message this channel · @ to mention") prop:value=move||draft.get()
                                disabled=move||!chat.permissions.with(|p|p.contains(&Permission::SendMessages))
                                on:input=move|ev|{draft.set(event_target_value(&ev));chat.typing.set(true);mentions.input();}/>
                        </label>
                    </div>
                    <button class="icon-button self-end bg-brand" aria-label=move ||if editing.get().is_some(){crate::i18n::t("Save message")}else{crate::i18n::t("Send message")} data-tip=move ||if editing.get().is_some(){crate::i18n::t("Save")}else{crate::i18n::t("Send")} disabled=move||loading.get()||!composer_ready()||!chat.permissions.with(|p|p.contains(&Permission::SendMessages))><Icon kind=Glyph::Send/></button>
                </form>
            </footer>
        </section>
    }
}
