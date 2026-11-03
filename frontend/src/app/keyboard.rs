//! App-wide keyboard navigation and screen-reader announcements: the quick
//! switcher (Ctrl/Cmd+K), the shortcut reference (Ctrl/Cmd+/), channel/server
//! cycling, Escape to mark the open channel read, roving focus for lists and
//! grids, and one polite live region. Matching and index logic lives in
//! `thiscord_frontend::keyboard` so it is unit-tested natively.
use super::Ui;
use super::icons::{Glyph, Icon};
use leptos::prelude::*;
use std::collections::HashMap;
use thiscord_frontend::keyboard::{self as nav, Candidate, Command, Keys, Kind};
use thiscord_shared::{
    GuildId,
    friends::Relationship,
    ids::ConversationId,
    notifications::Target as Muted,
    permissions::{Channel, ChannelKind},
};
use wasm_bindgen::JsCast;

/// Quick-switcher results shown at once; typing narrows the rest.
const RESULT_LIMIT: usize = 50;
/// Characters of a new message read aloud; the body stays in the transcript.
const PREVIEW_LIMIT: usize = 140;

#[derive(Clone, Copy)]
pub(super) struct State {
    switcher: RwSignal<bool>,
    help: RwSignal<bool>,
    /// Channel lists of servers opened this session, so the switcher can offer
    /// them. Selecting one still re-reads (and re-authorizes) the server.
    channels: StoredValue<HashMap<GuildId, Vec<Channel>>>,
    announcement: RwSignal<String>,
    generation: StoredValue<u64>,
    /// New-message announcements waiting for the batching delay.
    queued: StoredValue<(usize, String)>,
}
impl State {
    pub(super) fn new() -> Self {
        Self {
            switcher: RwSignal::new(false),
            help: RwSignal::new(false),
            channels: StoredValue::new(HashMap::new()),
            announcement: RwSignal::new(String::new()),
            generation: StoredValue::new(0),
            queued: StoredValue::new((0, String::new())),
        }
    }
}

/// Reads `text` through the polite live region. Repeated text is announced
/// again; the region clears itself so stale text is not re-read later.
pub(super) fn announce(ui: Ui, text: String) {
    let state = ui.keyboard;
    let generation = state.generation.get_value().wrapping_add(1);
    state.generation.set_value(generation);
    state.announcement.set(String::new());
    leptos::task::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(60).await;
        if state.generation.try_get_value() != Some(generation) {
            return;
        }
        state.announcement.set(text);
        gloo_timers::future::TimeoutFuture::new(7000).await;
        if state.generation.try_get_value() == Some(generation) {
            state.announcement.set(String::new());
        }
    });
}

/// Announces a message that just arrived in the focused channel. Bursts are
/// summarized: one announcement per second at most, with a count.
pub(super) fn announce_message(ui: Ui, author: &str, body: &str, attachments: usize) {
    if !document().has_focus().unwrap_or(false) {
        return;
    }
    let mut preview = nav::preview(body, PREVIEW_LIMIT);
    if preview.is_empty() && attachments > 0 {
        preview = crate::i18n::t("Attachment").to_owned();
    }
    let latest = if preview.is_empty() {
        author.to_owned()
    } else {
        format!("{author}: {preview}")
    };
    let state = ui.keyboard;
    let first = state.queued.with_value(|(count, _)| *count == 0);
    state.queued.update_value(|(count, text)| {
        *count += 1;
        *text = latest;
    });
    if !first {
        return;
    }
    leptos::task::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(1000).await;
        let Some((count, latest)) = state.queued.try_update_value(std::mem::take) else {
            return;
        };
        announce(
            ui,
            if count > 1 {
                thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "{count} new messages. Latest from {latest}",
                    count = count,
                    latest = latest
                )
            } else {
                latest
            },
        );
    });
}

fn apple() -> bool {
    window()
        .navigator()
        .user_agent()
        .is_ok_and(|agent| agent.contains("Macintosh"))
}

fn element(target: Option<web_sys::EventTarget>) -> Option<web_sys::Element> {
    target.and_then(|t| t.dyn_into::<web_sys::Element>().ok())
}

fn html(element: Option<web_sys::Element>) -> Option<web_sys::HtmlElement> {
    element.and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
}

fn visible(element: &web_sys::Element) -> bool {
    let rect = element.get_bounding_client_rect();
    rect.width() > 0.0 || rect.height() > 0.0
}

/// Focusable, visible items matching `selector` inside `container`.
fn items(container: &web_sys::Element, selector: &str) -> Vec<web_sys::HtmlElement> {
    let Ok(nodes) = container.query_selector_all(selector) else {
        return vec![];
    };
    (0..nodes.length())
        .filter_map(|i| html(nodes.get(i).and_then(|n| n.dyn_into().ok())))
        .filter(|e| visible(e) && !e.has_attribute("disabled"))
        .collect()
}

/// ↑/↓/Home/End move focus among `[data-rove]` items of the list that handles
/// this event (marked `data-rove-list`). Only acts while an item itself has
/// focus, so inputs and nested controls keep their keys.
pub(super) fn rove(e: &web_sys::KeyboardEvent) {
    if e.default_prevented() || e.alt_key() || e.ctrl_key() || e.meta_key() || e.shift_key() {
        return;
    }
    let (Some(list), Some(target)) = (element(e.current_target()), element(e.target())) else {
        return;
    };
    if !target.has_attribute("data-rove")
        || target.closest("[data-rove-list]").ok().flatten().as_ref() != Some(&list)
    {
        return;
    }
    let items: Vec<_> = items(&list, "[data-rove]")
        .into_iter()
        .filter(|item| item.closest("[data-rove-list]").ok().flatten().as_ref() == Some(&list))
        .collect();
    let current = items.iter().position(|item| **item == target);
    if let Some(next) = nav::list_step(items.len(), current, &e.key()) {
        e.prevent_default();
        let _ = items[next].focus();
    }
}

/// ↑/↓/Home/End move focus among `selector` items of a small menu-like
/// group (the event's current target).
pub(super) fn rove_items(e: &web_sys::KeyboardEvent, selector: &str) {
    if e.default_prevented() || e.alt_key() || e.ctrl_key() || e.meta_key() || e.shift_key() {
        return;
    }
    let (Some(group), Some(target)) = (element(e.current_target()), element(e.target())) else {
        return;
    };
    let items = items(&group, selector);
    let Some(current) = items.iter().position(|item| **item == target) else {
        return;
    };
    if let Some(next) = nav::list_step(items.len(), Some(current), &e.key()) {
        e.prevent_default();
        let _ = items[next].focus();
    }
}

/// Arrow keys move focus spatially among `selector` items of a wrapped grid
/// (the event's current target); Home/End jump. Grids marked
/// `data-single-stop` (see [`single_tab_stop`]) move their one Tab stop along.
pub(super) fn rove_grid(e: &web_sys::KeyboardEvent, selector: &str) {
    if e.default_prevented() || e.alt_key() || e.ctrl_key() || e.meta_key() {
        return;
    }
    let (Some(grid), Some(target)) = (element(e.current_target()), element(e.target())) else {
        return;
    };
    let items = items(&grid, selector);
    let Some(current) = items.iter().position(|item| **item == target) else {
        return;
    };
    let centers: Vec<_> = items
        .iter()
        .map(|item| {
            let r = item.get_bounding_client_rect();
            (r.left() + r.width() / 2.0, r.top() + r.height() / 2.0)
        })
        .collect();
    if let Some(next) = nav::spatial_step(&centers, current, &e.key()) {
        e.prevent_default();
        if grid.has_attribute("data-single-stop") {
            let _ = target.set_attribute("tabindex", "-1");
            let _ = items[next].set_attribute("tabindex", "0");
        }
        let _ = items[next].focus();
    }
}

/// Radio groups: arrow keys move to and select the previous/next radio,
/// wrapping, as native radio buttons do.
pub(super) fn rove_radio(e: &web_sys::KeyboardEvent) {
    if e.default_prevented() || e.alt_key() || e.ctrl_key() || e.meta_key() {
        return;
    }
    let forward = match e.key().as_str() {
        "ArrowDown" | "ArrowRight" => true,
        "ArrowUp" | "ArrowLeft" => false,
        _ => return,
    };
    let (Some(group), Some(target)) = (element(e.current_target()), element(e.target())) else {
        return;
    };
    let radios = items(&group, "[role=radio]");
    let current = radios.iter().position(|item| **item == target);
    if current.is_none() {
        return;
    }
    if let Some(next) = nav::cycle(radios.len(), current, forward, |_| true) {
        e.prevent_default();
        let _ = radios[next].focus();
        radios[next].click();
    }
}

/// Gives a static grid one Tab stop: its first item (call once it renders).
pub(super) fn single_tab_stop(grid: &web_sys::Element, selector: &str) {
    for (index, item) in items(grid, selector).iter().enumerate() {
        let _ = item.set_attribute("tabindex", if index == 0 { "0" } else { "-1" });
    }
}

/// One Tab stop for a dynamic list: the item focused last, else the selected
/// one, else the first. Arrow keys ([`rove`]) move between the others.
#[derive(Clone, Copy)]
pub(super) struct Roving<K: Copy + PartialEq + Send + Sync + 'static> {
    focused: RwSignal<Option<K>>,
    stop: Memo<Option<K>>,
}
impl<K: Copy + PartialEq + Send + Sync + 'static> Roving<K> {
    pub(super) fn new(
        keys: impl Fn() -> Vec<K> + Send + Sync + 'static,
        selected: impl Fn() -> Option<K> + Send + Sync + 'static,
    ) -> Self {
        let focused = RwSignal::new(None::<K>);
        let stop = Memo::new(move |_| {
            let keys = keys();
            focused
                .get()
                .filter(|k| keys.contains(k))
                .or_else(|| selected().filter(|k| keys.contains(k)))
                .or_else(|| keys.first().copied())
        });
        Self { focused, stop }
    }
    pub(super) fn tabindex(self, key: K) -> impl Fn() -> &'static str + Copy + 'static {
        move || {
            if self.stop.get() == Some(key) {
                "0"
            } else {
                "-1"
            }
        }
    }
    pub(super) fn focus(self, key: K) {
        if self.focused.get_untracked() != Some(key) {
            self.focused.set(Some(key));
        }
    }
}

/// After keyboard navigation replaces the content that had focus, keep focus
/// in the conversation: its composer when present, else the main region.
fn settle_focus() {
    request_animation_frame(move || {
        request_animation_frame(move || {
            let lost = document()
                .active_element()
                .is_none_or(|a| document().body().is_some_and(|b| a == *b) || !a.is_connected());
            if !lost {
                return;
            }
            for id in ["chat-composer", "private-composer", "app-content"] {
                if let Some(target) = html(document().get_element_by_id(id))
                    && visible(&target)
                    && !target.has_attribute("disabled")
                {
                    let _ = target.focus();
                    return;
                }
            }
        });
    });
}

fn unread(ui: Ui, guild: GuildId, channel: &Channel) -> bool {
    channel.kind == ChannelKind::Text
        && ui.unread.with_untracked(|unread| {
            unread
                .iter()
                .any(|u| u.channel_id == channel.id && u.count > 0)
        })
        && !super::notifications::muted(
            ui,
            Muted::Channel {
                guild_id: guild,
                channel_id: channel.id,
            },
        )
}

fn conversations(ui: Ui) -> Vec<(ConversationId, String, bool)> {
    ui.friends.snapshot.with_untracked(|s| {
        s.friends
            .iter()
            .filter(|f| f.relationship == Relationship::Friend)
            .map(|f| {
                let unread = f.unread_count > 0
                    && !super::notifications::muted(
                        ui,
                        Muted::Conversation {
                            conversation_id: f.conversation_id,
                        },
                    );
                (f.conversation_id, f.person.display_name.clone(), unread)
            })
            .collect()
    })
}

/// Alt+↑/↓ (and with Shift, unread only) through text channels of the open
/// server, or private conversations on the home page.
fn cycle_channel(ui: Ui, forward: bool, unread_only: bool) {
    if ui.navigation.get_untracked().pending {
        return;
    }
    if let Some(home) = ui.server.get_untracked() {
        let pinned = ui.sidebar_preferences.with_untracked(|p| p.pinned.clone());
        let ordered = nav::channel_order(&home.channels, &home.categories, &pinned);
        let current = ui.selected_channel.with_untracked(|selected| {
            selected
                .as_ref()
                .and_then(|s| ordered.iter().position(|c| c.id == s.id))
        });
        let guild = home.guild.id;
        let next = nav::cycle(ordered.len(), current, forward, |i| {
            let channel = ordered[i];
            channel.kind == ChannelKind::Text && (!unread_only || unread(ui, guild, channel))
        });
        match next {
            Some(index) => {
                let channel = ordered[index].clone();
                announce(ui, format!("# {}", channel.name));
                ui.selected_channel.set(Some(channel));
                ui.mobile_content.set(true);
                settle_focus();
            }
            None if unread_only => announce(ui, crate::i18n::t("No other unread channels").into()),
            None => {}
        }
        return;
    }
    if ui.navigation.get_untracked().current().guild.is_some() {
        return;
    }
    let list = conversations(ui);
    let selected = ui.friends.selected.get_untracked();
    let current = list.iter().position(|(id, _, _)| Some(*id) == selected);
    match nav::cycle(list.len(), current, forward, |i| !unread_only || list[i].2) {
        Some(index) => {
            let (id, name, _) = list[index].clone();
            announce(ui, name);
            super::friends::open(ui, id);
            settle_focus();
        }
        None if unread_only => announce(ui, crate::i18n::t("No other unread conversations").into()),
        None => {}
    }
}

/// Ctrl+Alt+↑/↓ through Home and the joined servers, in rail order.
fn cycle_server(ui: Ui, forward: bool) {
    let guilds = ui.guilds.get_untracked();
    let current = match ui.navigation.get_untracked().current().guild {
        Some(id) => guilds.iter().position(|g| g.id == id).map(|i| i + 1),
        None => Some(0),
    };
    let Some(next) = nav::cycle(guilds.len() + 1, current, forward, |_| true) else {
        return;
    };
    if next == 0 {
        announce(ui, crate::i18n::t("Thiscord home").into());
        super::servers::home(ui);
    } else {
        let guild = &guilds[next - 1];
        announce(ui, guild.name.clone());
        super::servers::navigate(ui, guild.id);
    }
    settle_focus();
}

fn run(ui: Ui, command: Command) {
    let state = ui.keyboard;
    match command {
        Command::QuickSwitcher => {
            state.help.set(false);
            state.switcher.update(|open| *open = !*open);
        }
        Command::ShortcutHelp => {
            state.switcher.set(false);
            state.help.update(|open| *open = !*open);
        }
        Command::PreviousChannel => cycle_channel(ui, false, false),
        Command::NextChannel => cycle_channel(ui, true, false),
        Command::PreviousUnread => cycle_channel(ui, false, true),
        Command::NextUnread => cycle_channel(ui, true, true),
        Command::PreviousServer => cycle_server(ui, false),
        Command::NextServer => cycle_server(ui, true),
    }
}

fn keydown(ui: Ui, e: &web_sys::KeyboardEvent) {
    if e.is_composing() || e.default_prevented() {
        return;
    }
    let key = e.key();
    if key == "Tab" {
        super::dialog::wrap_tab(e);
        return;
    }
    if ui.account.with_untracked(Option::is_none) {
        return;
    }
    let modal = super::dialog::modal_open();
    if key == "Escape" {
        // Popovers, menus, search and dialogs handle Escape first; mark the
        // channel read only if none of them did.
        if !modal && ui.context_menu.with_untracked(Option::is_none) && !e.repeat() {
            let event = e.clone();
            set_timeout(
                move || {
                    if !event.default_prevented()
                        && let Some(chat) = ui.active_chat.get_untracked()
                        && chat.mark_read()
                    {
                        announce(ui, crate::i18n::t("Marked as read").into());
                    }
                },
                std::time::Duration::ZERO,
            );
        }
        return;
    }
    let code = e.code();
    let typing = super::audio::editable(e.target());
    let keys = Keys {
        key: &key,
        code: &code,
        control: e.ctrl_key(),
        shift: e.shift_key(),
        alt: e.alt_key(),
        meta: e.meta_key(),
    };
    let Some(command) = Command::from_keys(&keys, typing) else {
        return;
    };
    // Mute/deafen bindings chosen in this browser take precedence.
    if crate::account_client::web_app() {
        let pressed = thiscord_frontend::web_shortcuts::Pressed {
            code: &code,
            control: keys.control,
            shift: keys.shift,
            alt: keys.alt,
            meta: keys.meta,
        };
        if crate::browser_voice::shortcut_bindings()
            .action(&pressed, typing)
            .is_some()
        {
            return;
        }
    }
    let state = ui.keyboard;
    if modal {
        // A modal owns the keyboard; only the switcher and help toggle closed.
        let own = match command {
            Command::QuickSwitcher => state.switcher.get_untracked(),
            Command::ShortcutHelp => state.help.get_untracked(),
            _ => false,
        };
        if own {
            e.prevent_default();
            run(ui, command);
        }
        return;
    }
    if e.repeat() && matches!(command, Command::QuickSwitcher | Command::ShortcutHelp) {
        return;
    }
    e.prevent_default();
    run(ui, command);
}

/// Global shortcuts, the quick switcher, the shortcut reference and the live
/// region. Mounted once inside the signed-in app shell.
#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let listener = window_event_listener(leptos::ev::keydown, move |e| keydown(ui, &e));
    on_cleanup(move || listener.remove());
    // Remember channel lists for the switcher; forget them with the session.
    Effect::new(move |_| {
        if let Some(home) = ui.server.get() {
            ui.keyboard.channels.update_value(|cache| {
                cache.insert(home.guild.id, home.channels.clone());
            });
        }
    });
    Effect::new(move |_| {
        ui.token.track();
        ui.keyboard.channels.update_value(HashMap::clear);
    });
    // Connection and voice state changes are announced once each.
    Effect::new(move |previous: Option<super::chat::ConnectionStatus>| {
        let status = ui.socket_status.get();
        if previous.is_some_and(|p| p != status)
            && matches!(
                status,
                super::chat::ConnectionStatus::Connected
                    | super::chat::ConnectionStatus::Reconnecting
            )
            && ui.server.with_untracked(Option::is_some)
        {
            announce(ui, status.label().to_owned());
        }
        status
    });
    let voice = Memo::new(move |_| ui.voice.with(|v| v.channel_id));
    Effect::new(
        move |previous: Option<Option<thiscord_shared::ChannelId>>| {
            let current = voice.get();
            if let Some(previous) = previous
                && previous != current
            {
                announce(
                    ui,
                    if current.is_some() {
                        crate::i18n::t("Voice connected").into()
                    } else {
                        crate::i18n::t("Voice disconnected").into()
                    },
                );
            }
            current
        },
    );
    view! {
        <div class="sr-only" role="status" aria-live="polite" aria-atomic="true">{move || ui.keyboard.announcement.get()}</div>
        <QuickSwitcher ui/>
        <ShortcutHelp ui/>
    }
}

#[derive(Clone, PartialEq)]
enum Destination {
    Server(GuildId),
    Channel(GuildId, thiscord_shared::ChannelId),
    Conversation(ConversationId),
}

fn destinations(ui: Ui) -> Vec<(Candidate, Destination)> {
    let mut out = vec![];
    let current_guild = ui.navigation.with(|n| n.current().guild);
    let guilds = ui.guilds.get();
    let loaded = ui.server.with(|s| s.as_ref().map(|s| s.guild.id));
    let selected = ui.selected_channel.with(|c| c.as_ref().map(|c| c.id));
    let pinned = ui.sidebar_preferences.with(|p| p.pinned.clone());
    ui.unread.track();
    for guild in &guilds {
        out.push((
            Candidate {
                kind: Kind::Server,
                label: guild.name.clone(),
                context: String::new(),
                unread: false,
                current: current_guild == Some(guild.id),
            },
            Destination::Server(guild.id),
        ));
    }
    // The open server first, then others seen this session, in rail order.
    let mut order: Vec<&thiscord_shared::permissions::Guild> = guilds.iter().collect();
    order.sort_by_key(|g| Some(g.id) != loaded);
    for guild in order {
        let channels = if Some(guild.id) == loaded {
            ui.server.with(|s| {
                s.as_ref()
                    .map(|s| (s.channels.clone(), s.categories.clone()))
            })
        } else {
            ui.keyboard
                .channels
                .with_value(|cache| cache.get(&guild.id).map(|c| (c.clone(), vec![])))
        };
        let Some((channels, categories)) = channels else {
            continue;
        };
        for channel in nav::channel_order(&channels, &categories, &pinned) {
            let voice = channel.kind == ChannelKind::Voice;
            out.push((
                Candidate {
                    kind: if voice {
                        Kind::VoiceChannel
                    } else {
                        Kind::TextChannel
                    },
                    label: channel.name.clone(),
                    context: guild.name.clone(),
                    unread: Some(guild.id) == loaded && unread(ui, guild.id, channel),
                    current: Some(guild.id) == loaded && selected == Some(channel.id),
                },
                Destination::Channel(guild.id, channel.id),
            ));
        }
    }
    let open_conversation = ui
        .friends
        .selected
        .get()
        .filter(|_| current_guild.is_none());
    ui.friends.snapshot.track();
    for (id, name, unread) in conversations(ui) {
        out.push((
            Candidate {
                kind: Kind::Conversation,
                label: name,
                context: crate::i18n::t("Direct message").into(),
                unread,
                current: open_conversation == Some(id),
            },
            Destination::Conversation(id),
        ));
    }
    out
}

fn open_channel(ui: Ui, guild: GuildId, channel: thiscord_shared::ChannelId) {
    let pick = move || {
        let Some(home) = ui.server.get_untracked().filter(|h| h.guild.id == guild) else {
            return;
        };
        let Some(found) = home.channels.iter().find(|c| c.id == channel).cloned() else {
            ui.status.set("That channel is no longer available".into());
            return;
        };
        if found.kind == ChannelKind::Voice {
            super::audio::join(ui, home.guild, found);
        } else {
            ui.selected_channel.set(Some(found));
            ui.mobile_content.set(true);
        }
        settle_focus();
    };
    let ready = !ui.navigation.get_untracked().pending
        && ui
            .server
            .with_untracked(|s| s.as_ref().is_some_and(|s| s.guild.id == guild));
    if ready {
        pick();
        return;
    }
    let selection = super::servers::select(ui, guild);
    leptos::task::spawn_local(async move {
        match selection.await {
            Ok(true) => pick(),
            Ok(false) => {}
            Err(error) => ui.status.set(error),
        }
    });
}

fn go(ui: Ui, destination: Destination) {
    match destination {
        Destination::Server(id) => {
            super::servers::navigate(ui, id);
            settle_focus();
        }
        Destination::Channel(guild, channel) => open_channel(ui, guild, channel),
        Destination::Conversation(id) => {
            super::friends::open(ui, id);
            settle_focus();
        }
    }
}

/// Ctrl/Cmd+K: an accessible combobox over servers, channels and friends.
#[component]
fn QuickSwitcher(ui: Ui) -> impl IntoView {
    let state = ui.keyboard;
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let input = NodeRef::<leptos::html::Input>::new();
    let query = RwSignal::new(String::new());
    let active = RwSignal::new(0_usize);
    let results = Memo::new(move |_| {
        if !state.switcher.get() {
            return vec![];
        }
        let all = destinations(ui);
        let candidates: Vec<_> = all.iter().map(|(c, _)| c.clone()).collect();
        let ranked = query.with(|q| nav::rank(q, &candidates, RESULT_LIMIT));
        ranked
            .into_iter()
            .map(|i| all[i].clone())
            .collect::<Vec<_>>()
    });
    Effect::new(move |_| {
        query.track();
        active.set(0);
    });
    Effect::new(move |_| {
        let open = state.switcher.get();
        let Some(element) = dialog.get() else {
            return;
        };
        if open {
            query.set(String::new());
            super::dialog::open(&element);
            if let Some(input) = input.get_untracked() {
                let _ = input.focus();
            }
        } else if element.open() {
            element.close();
        }
    });
    // Keep the active option visible while arrowing through a long list.
    Effect::new(move |_| {
        let index = active.get();
        if let Some(option) =
            document().get_element_by_id(&format!("quick-switcher-option-{index}"))
            && let Some(list) = document().get_element_by_id("quick-switcher-results")
        {
            let (o, l) = (
                option.get_bounding_client_rect(),
                list.get_bounding_client_rect(),
            );
            if o.top() < l.top() {
                option.scroll_into_view_with_bool(true);
            } else if o.bottom() > l.bottom() {
                option.scroll_into_view_with_bool(false);
            }
        }
    });
    let choose = move |index: usize| {
        let Some((_, destination)) = results.with_untracked(|r| r.get(index).cloned()) else {
            return;
        };
        state.switcher.set(false);
        go(ui, destination);
    };
    let keydown = move |e: web_sys::KeyboardEvent| {
        if e.is_composing() {
            return;
        }
        let count = results.with_untracked(Vec::len);
        match e.key().as_str() {
            key @ ("ArrowDown" | "ArrowUp") => {
                e.prevent_default();
                if let Some(next) = nav::list_step(count, Some(active.get_untracked()), key) {
                    active.set(next);
                }
            }
            "PageDown" | "PageUp" if count > 0 => {
                e.prevent_default();
                let current = active.get_untracked();
                active.set(if e.key() == "PageDown" {
                    (current + 8).min(count - 1)
                } else {
                    current.saturating_sub(8)
                });
            }
            "Enter" if !e.repeat() => {
                e.prevent_default();
                choose(active.get_untracked());
            }
            _ => {}
        }
    };
    let option_id = |index: usize| format!("quick-switcher-option-{index}");
    view! {
        <dialog node_ref=dialog class="context-dialog quick-switcher" aria-labelledby="quick-switcher-title"
            on:close=move |_| state.switcher.set(false)
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, false)>
            <h2 id="quick-switcher-title" class="text-lg font-semibold">{move || crate::i18n::t("Quick switcher")}</h2>
            <p id="quick-switcher-hint" class="mt-1 text-sm text-white/70">{move || crate::i18n::t("Search servers, channels and friends. ↑↓ to choose, Enter to open, Esc to close.")}</p>
            <input node_ref=input type="text" class="quick-switcher-input mt-4" role="combobox" autocomplete="off" spellcheck="false"
                aria-labelledby="quick-switcher-title" aria-describedby="quick-switcher-hint" aria-autocomplete="list"
                aria-controls="quick-switcher-results" aria-expanded=move || (!results.with(Vec::is_empty)).to_string()
                aria-activedescendant=move || results.with(|r| (!r.is_empty()).then(|| option_id(active.get())))
                placeholder=move || crate::i18n::t("Where would you like to go?")
                prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) on:keydown=keydown/>
            <ul id="quick-switcher-results" role="listbox" class="quick-switcher-results" aria-label=move || crate::i18n::t("Results")>
                {move || results.get().into_iter().enumerate().map(|(index, (candidate, _))| {
                    let icon = match candidate.kind {
                        Kind::Server => view! {<Icon kind=Glyph::Compass/>}.into_any(),
                        Kind::TextChannel => view! {<span class="w-5 text-center text-lg" aria-hidden="true">"#"</span>}.into_any(),
                        Kind::VoiceChannel => view! {<Icon kind=Glyph::Volume/>}.into_any(),
                        Kind::Conversation => view! {<Icon kind=Glyph::Person/>}.into_any(),
                    };
                    let kind = match candidate.kind {
                        Kind::Server => "Server",
                        Kind::TextChannel => "Text channel",
                        Kind::VoiceChannel => "Voice channel",
                        Kind::Conversation => "Direct message",
                    };
                    let (unread, kind_shown, context) = (candidate.unread, candidate.kind, candidate.context.clone());
                    view! {
                        <li id=option_id(index) role="option" class="quick-switcher-option"
                            aria-selected=move || (active.get() == index).to_string()
                            on:pointerdown=move |e| e.prevent_default()
                            on:pointermove=move |_| if active.get_untracked() != index { active.set(index); }
                            on:click=move |_| choose(index)>
                            <span class="shrink-0 text-muted" aria-hidden="true">{icon}</span>
                            <span class="min-w-0 flex-1 truncate">{candidate.label.clone()}</span>
                            <span class="sr-only">{move || format!(", {}", crate::i18n::t(kind))}</span>
                            <Show when=move || unread><span class="quick-switcher-unread">{move || crate::i18n::t("Unread")}</span></Show>
                            <Show when=move || matches!(kind_shown, Kind::TextChannel | Kind::VoiceChannel)>
                                <span class="max-w-[40%] shrink-0 truncate text-xs text-muted">{context.clone()}</span>
                            </Show>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <p class="mt-3 text-sm text-muted" role="status">{move || {
                let count = results.with(Vec::len);
                if count == 0 { crate::i18n::t("No matches").to_owned() }
                else { thiscord_frontend::localized_format!(crate::i18n::language(), "{} results", count) }
            }}</p>
        </dialog>
    }
}

/// The navigation and chat shortcut table (help dialog and settings page).
#[component]
pub(super) fn ShortcutReference() -> impl IntoView {
    let apple = apple();
    view! {
        <div class="shortcut-reference">
            {nav::SHORTCUTS.iter().enumerate().map(|(index, &(group, rows))| view! {
                <section aria-labelledby=format!("shortcut-group-{index}")>
                    <h3 id=format!("shortcut-group-{index}") class="shortcut-group">{move || crate::i18n::t(group)}</h3>
                    <dl class="shortcut-list">
                        {rows.iter().map(|row| view! {
                            <div class="shortcut-row">
                                <dt>{move || crate::i18n::t(row.action)}</dt>
                                <dd><kbd>{nav::display_keys(row.keys, apple)}</kbd></dd>
                            </div>
                        }).collect_view()}
                    </dl>
                </section>
            }).collect_view()}
        </div>
    }
}

/// Ctrl/Cmd+/: the shortcut reference in a dialog.
#[component]
fn ShortcutHelp(ui: Ui) -> impl IntoView {
    let state = ui.keyboard;
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        let open = state.help.get();
        let Some(element) = dialog.get() else {
            return;
        };
        if open {
            super::dialog::open(&element);
        } else if element.open() {
            element.close();
        }
    });
    view! {
        <dialog node_ref=dialog class="context-dialog shortcut-dialog" aria-labelledby="shortcut-help-title"
            on:close=move |_| state.help.set(false)
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, false)>
            <header class="mb-4 flex items-center justify-between gap-4">
                <h2 id="shortcut-help-title" class="text-xl font-semibold">{move || crate::i18n::t("Keyboard shortcuts")}</h2>
                <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Close keyboard shortcuts")
                    on:click=move |_| state.help.set(false)><Icon kind=Glyph::Close/></button>
            </header>
            <Show when=move || state.help.get()><ShortcutReference/></Show>
        </dialog>
    }
}
