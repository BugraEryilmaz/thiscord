//! Guild member list: a right-side panel on wide layouts, a drawer on phones and
//! tablets, and a profile card for each member. Data arrives on the chat socket.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_frontend::member_list::{PANEL_KEY, apply_presence, grouped, status_text};
use thiscord_shared::{
    AccountId, AvatarId, GuildId,
    chat::{CHAT_PATH, ChatRequest, ChatResponse, GuildMember, MemberProfile},
    friends::Relationship,
};
use wasm_bindgen::JsCast;

/// Layouts narrower than this show the list as an overlay drawer; matches the
/// `max-width: 1023px` media query in style.css (both use the layout viewport).
const DRAWER_BELOW: f64 = 1024.0;

#[derive(Clone, Copy)]
pub(super) struct State {
    /// The latest authorized list for `guild`, as delivered by the socket.
    list: RwSignal<Vec<GuildMember>>,
    guild: RwSignal<Option<GuildId>>,
    /// Wide layouts: side panel, persisted on this device.
    panel: RwSignal<bool>,
    /// Narrow layouts: temporary drawer, closed on navigation.
    drawer: RwSignal<bool>,
    narrow: RwSignal<bool>,
    profile: RwSignal<Option<(GuildId, AccountId)>>,
    /// Element that opened the profile card; focus returns there.
    opener: StoredValue<Option<web_sys::HtmlElement>, LocalStorage>,
}

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}
fn narrow() -> bool {
    window()
        .inner_width()
        .ok()
        .and_then(|width| width.as_f64())
        .is_some_and(|width| width < DRAWER_BELOW)
}

impl State {
    pub(super) fn new() -> Self {
        let panel = storage()
            .and_then(|s| s.get_item(PANEL_KEY).ok().flatten())
            .is_none_or(|value| value != "0");
        Self {
            list: RwSignal::new(vec![]),
            guild: RwSignal::new(None),
            panel: RwSignal::new(panel),
            drawer: RwSignal::new(false),
            narrow: RwSignal::new(narrow()),
            profile: RwSignal::new(None),
            opener: StoredValue::new_local(None),
        }
    }
    pub(super) fn guild(self) -> Option<GuildId> {
        self.guild.get_untracked()
    }
    /// Drops the list on guild changes, disconnects and access loss.
    pub(super) fn clear(self) {
        self.guild.set(None);
        self.list.set(vec![]);
    }
    pub(super) fn replace(self, guild: Option<GuildId>, members: Vec<GuildMember>) {
        self.guild.set(guild);
        self.list.set(members);
    }
    pub(super) fn presence(self, online: &[AccountId]) {
        self.list.update(|list| apply_presence(list, online));
    }
    /// Current status line of a listed member, for tooltips elsewhere.
    pub(super) fn status(self, guild: GuildId, id: AccountId) -> Option<String> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.list.with(|list| {
            list.iter()
                .find(|m| m.account_id == id)
                .and_then(|m| m.custom_status.as_ref())
                .map(status_text)
        })
    }
    pub(super) fn name(self, guild: GuildId, id: AccountId) -> Option<String> {
        if self.guild.get_untracked() != Some(guild) {
            return None;
        }
        self.list.with_untracked(|list| {
            list.iter()
                .find(|m| m.account_id == id)
                .map(|m| m.display_name().to_owned())
        })
    }
    fn toggle(self) {
        // Same layout source as `open`, so the button always flips what is shown.
        if self.narrow.get_untracked() {
            self.drawer.update(|open| *open = !*open);
        } else {
            let open = !self.panel.get_untracked();
            self.panel.set(open);
            if let Some(storage) = storage() {
                let _ = storage.set_item(PANEL_KEY, if open { "1" } else { "0" });
            }
        }
    }
    fn open(self) -> bool {
        if self.narrow.get() {
            self.drawer.get()
        } else {
            self.panel.get()
        }
    }
}

/// Header button showing or hiding the member list.
#[component]
pub(super) fn Toggle(ui: Ui) -> impl IntoView {
    view! {
        <button type="button" class="icon-button shrink-0" aria-controls="member-list"
            aria-expanded=move || ui.members.open().to_string()
            aria-label=move || crate::i18n::t("Member list") data-tip=move || crate::i18n::t("Member list")
            on:click=move |_| ui.members.toggle()><Icon kind=Glyph::Person/></button>
    }
}

/// Key of a rendered row; any displayed change re-creates that row only.
type RowKey = (
    AccountId,
    String,
    Option<AvatarId>,
    String,
    bool,
    bool,
    bool,
);
fn row_key(member: &GuildMember) -> RowKey {
    (
        member.account_id,
        member.display_name().to_owned(),
        member.avatar_id,
        member
            .custom_status
            .as_ref()
            .map(status_text)
            .unwrap_or_default(),
        member.online,
        member.owner,
        member.bot,
    )
}

#[component]
fn Row(
    ui: Ui,
    guild: GuildId,
    member: GuildMember,
    roving: super::keyboard::Roving<thiscord_shared::AccountId>,
) -> impl IntoView {
    let id = member.account_id;
    let name = member.display_name().to_owned();
    let status = member.custom_status.as_ref().map(status_text);
    let own = move || ui.account.with(|a| a.as_ref().is_some_and(|a| a.id == id));
    let online = member.online;
    let label = {
        let name = name.clone();
        let status = status.clone();
        move || {
            let presence = if own() && super::notifications::do_not_disturb(ui) {
                crate::i18n::t("Do not disturb")
            } else if online {
                crate::i18n::t("Online")
            } else {
                crate::i18n::t("Offline")
            };
            let mut label = format!("{name}, {presence}");
            if member.owner {
                label = format!("{label}, {}", crate::i18n::t("Server owner"));
            }
            if let Some(status) = &status {
                label = format!("{label}, {status}");
            }
            label
        }
    };
    let avatar_name = name.clone();
    view! {
        <li>
            <button type="button" class="member-row" class:member-offline=!online
                data-rove tabindex=roving.tabindex(id) on:focus=move |_| roving.focus(id)
                data-context-server=guild.to_string() data-context-member=id.to_string()
                aria-haspopup="dialog" aria-label=label
                on:click=move |ev| {
                    let opener = ev.current_target().and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok());
                    ui.members.opener.set_value(opener);
                    ui.members.profile.set(Some((guild, id)));
                }>
                <span class="relative shrink-0">
                    <crate::avatar::Avatar class="size-8 text-xs" name=avatar_name avatar_id=member.avatar_id/>
                    <span class="presence-dot" class:online=online
                        class:dnd=move || online && own() && super::notifications::do_not_disturb(ui) aria-hidden="true"></span>
                </span>
                <span class="min-w-0 flex-1">
                    <span class="flex min-w-0 items-center gap-1">
                        <span class="truncate text-sm font-medium">{name}</span>
                        {member.owner.then(|| view! {<span class="member-badge">{move || crate::i18n::t("Owner")}</span>})}
                        {member.bot.then(|| view! {<span class="member-badge">{move || crate::i18n::t("Bot")}</span>})}
                    </span>
                    {status.map(|status| view! {<span class="block truncate text-xs text-white/60">{status}</span>})}
                </span>
            </button>
        </li>
    }
}

#[component]
fn Group(
    ui: Ui,
    guild: GuildId,
    title: &'static str,
    heading: &'static str,
    members: Memo<Vec<GuildMember>>,
    roving: super::keyboard::Roving<thiscord_shared::AccountId>,
) -> impl IntoView {
    view! {
        <Show when=move || members.with(|m| !m.is_empty())>
            <section aria-labelledby=heading>
                <h3 id=heading class="member-group-heading">
                    {move || format!("{} — {}", crate::i18n::t(title), members.with(Vec::len))}
                </h3>
                <ul role="list" class="space-y-0.5">
                    <For each=move || members.get() key=row_key
                        children=move |member| view! {<Row ui guild member roving/>}/>
                </ul>
            </section>
        </Show>
    }
}

/// Member list for the open guild channel. Wide layouts show it beside the chat;
/// narrow ones show it as an overlay drawer with a backdrop.
#[component]
pub(super) fn Panel(ui: Ui, guild: GuildId) -> impl IntoView {
    // Resizes while no channel was open were not observed; resync on mount.
    ui.members.narrow.set(narrow());
    let resize = window_event_listener(leptos::ev::resize, move |_| {
        let now = narrow();
        if ui.members.narrow.get_untracked() != now {
            ui.members.narrow.set(now);
            ui.members.drawer.set(false);
        }
    });
    on_cleanup(move || {
        resize.remove();
        ui.members.drawer.set(false);
    });
    let list = Memo::new(move |_| {
        if ui.members.guild.get() == Some(guild) {
            ui.members.list.get()
        } else {
            vec![]
        }
    });
    let online =
        Memo::new(move |_| list.with(|l| grouped(l).0.into_iter().cloned().collect::<Vec<_>>()));
    let offline =
        Memo::new(move |_| list.with(|l| grouped(l).1.into_iter().cloned().collect::<Vec<_>>()));
    let roving = super::keyboard::Roving::new(
        move || {
            online
                .with(|on| offline.with(|off| on.iter().chain(off).map(|m| m.account_id).collect()))
        },
        || None,
    );
    // Opening the drawer moves focus into it; closing returns focus to the toggle.
    let close_button = NodeRef::<leptos::html::Button>::new();
    Effect::new(move |_| {
        if ui.members.narrow.get()
            && ui.members.drawer.get()
            && let Some(button) = close_button.get()
        {
            let _ = button.focus();
        }
    });
    let close = move || {
        ui.members.drawer.set(false);
        if let Some(button) = document()
            .query_selector("[aria-controls=member-list]")
            .ok()
            .flatten()
            .and_then(|b| b.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = button.focus();
        }
    };
    view! {
        <Show when=move || ui.members.narrow.get() && ui.members.drawer.get()>
            <div class="member-drawer-backdrop" aria-hidden="true" on:click=move |_| close()></div>
        </Show>
        <aside id="member-list" class="member-panel" aria-label=move || crate::i18n::t("Members")
            class:member-panel-open=move || ui.members.open()
            on:keydown=move |ev| if ev.key() == "Escape" && ui.members.narrow.get_untracked() && ui.members.drawer.get_untracked() {
                ev.prevent_default();
                close();
            }>
            <header class="member-panel-header">
                <h2 class="pane-title min-w-0 flex-1 truncate text-[15px]">{move || crate::i18n::t("Members")}</h2>
                <button node_ref=close_button type="button" class="icon-button member-drawer-close" aria-label=move || crate::i18n::t("Close member list")
                    on:click=move |_| close()><Icon kind=Glyph::Close/></button>
            </header>
            <div class="member-panel-scroll" data-rove-list on:keydown=move |e| super::keyboard::rove(&e)>
                <Show when=move || list.with(Vec::is_empty)>
                    <p class="px-2 py-4 text-sm text-white/60" role="status">{move ||
                        if ui.socket_status.get() == super::chat::ConnectionStatus::Connected { crate::i18n::t("Loading members…") }
                        else { ui.socket_status.get().label() }}</p>
                </Show>
                <Group ui guild title="Online" heading="member-group-online" members=online roving/>
                <Group ui guild title="Offline" heading="member-group-offline" members=offline roving/>
            </div>
        </aside>
    }
}

/// Profile card for a member selected in the list. Mounted once per chat view.
#[component]
pub(super) fn ProfileHost(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let profile = RwSignal::new(None::<MemberProfile>);
    let error = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    // Switching channels remounts this host; never reopen a card there.
    on_cleanup(move || ui.members.profile.set(None));
    Effect::new(move |_| {
        let target = ui.members.profile.get();
        let current = generation.get_untracked().wrapping_add(1);
        generation.set(current);
        profile.set(None);
        error.set(String::new());
        let Some((guild, id)) = target else {
            if let Some(dialog) = dialog.get_untracked() {
                dialog.close();
            }
            return;
        };
        if let Some(dialog) = dialog.get_untracked()
            && !dialog.open()
        {
            super::dialog::open(&dialog);
        }
        let token = ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let result = crate::account_client::api_request::<_, ChatResponse>(
                CHAT_PATH,
                &ChatRequest::MemberProfile {
                    guild_id: guild,
                    account_id: id,
                },
                token.as_deref(),
            )
            .await;
            if generation.try_get_untracked() != Some(current) {
                return;
            }
            match result {
                Ok(ChatResponse::MemberProfile { profile: card }) => profile.set(Some(card)),
                Ok(_) => error.set("Unexpected server response".into()),
                Err(e) => error.set(e),
            }
        });
    });
    // Leaving the server, signing out or losing the list closes the card.
    Effect::new(move |_| {
        ui.token.track();
        let guild = ui.server.with(|s| s.as_ref().map(|s| s.guild.id));
        if ui
            .members
            .profile
            .get_untracked()
            .is_some_and(|(g, _)| Some(g) != guild)
        {
            ui.members.profile.set(None);
        }
    });
    // "More actions…" hands focus to the context menu instead of the row.
    let refocus = StoredValue::new(true);
    let closed = move || {
        ui.members.profile.set(None);
        if refocus.get_value()
            && let Some(opener) = ui.members.opener.get_value()
            && opener.is_connected()
        {
            let _ = opener.focus();
        }
        refocus.set_value(true);
    };
    let close = move || {
        if let Some(dialog) = dialog.get_untracked() {
            dialog.close();
        }
    };
    let conversation = move || {
        let id = ui.members.profile.get()?.1;
        ui.friends.snapshot.with(|s| {
            s.friends
                .iter()
                .find(|f| f.person.id == id && f.relationship == Relationship::Friend)
                .map(|f| f.conversation_id)
        })
    };
    let own = move || {
        ui.members
            .profile
            .get()
            .is_some_and(|(_, id)| ui.account.with(|a| a.as_ref().is_some_and(|a| a.id == id)))
    };
    view! {
        <dialog node_ref=dialog class="context-dialog profile-card" aria-labelledby="profile-card-name"
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, false) on:close=move |_| closed()>
            <div class="flex items-start gap-4">
                <crate::avatar::Avatar class="size-16 text-2xl"
                    name=Signal::derive(move || profile.with(|p| p.as_ref().map(|p| p.display_name.clone()).unwrap_or_default()))
                    avatar_id=Signal::derive(move || profile.with(|p| p.as_ref().and_then(|p| p.avatar_id)))/>
                <div class="min-w-0 flex-1">
                    <h2 id="profile-card-name" class="break-words text-xl font-semibold">{move || profile.with(|p| p.as_ref().map(|p| p.display_name.clone()))}</h2>
                    <p class="break-all text-sm text-white/60">{move || profile.with(|p| p.as_ref().map(|p| format!("@{}", p.username)))}</p>
                    <Show when=move || profile.with(|p| p.as_ref().is_some_and(|p| p.owner || p.bot))>
                        <p class="mt-1 flex gap-1">
                            <Show when=move || profile.with(|p| p.as_ref().is_some_and(|p| p.owner))><span class="member-badge">{move || crate::i18n::t("Server owner")}</span></Show>
                            <Show when=move || profile.with(|p| p.as_ref().is_some_and(|p| p.bot))><span class="member-badge">{move || crate::i18n::t("Bot")}</span></Show>
                        </p>
                    </Show>
                </div>
                <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Close profile") on:click=move |_| close()><Icon kind=Glyph::Close/></button>
            </div>
            <Show when=move || profile.with(Option::is_none) && error.with(String::is_empty)>
                <p class="mt-4 text-sm text-white/60" role="status">{move || crate::i18n::t("Loading profile…")}</p>
            </Show>
            <p class="mt-4 text-sm text-red-300" role="alert" hidden=move || error.with(String::is_empty)>{move || crate::i18n::message(error.get())}</p>
            {move || profile.with(|p| p.as_ref().and_then(|p| p.custom_status.as_ref()).map(|status| {
                let text = status_text(status);
                view! {
                    <section class="mt-4" aria-labelledby="profile-card-status">
                        <h3 id="profile-card-status" class="text-xs font-semibold uppercase tracking-wider text-white/60">{move || crate::i18n::t("Custom status")}</h3>
                        <p class="mt-1 whitespace-pre-wrap break-words text-sm">{text}</p>
                    </section>
                }
            }))}
            <Show when=move || profile.with(|p| p.as_ref().is_some_and(|p| !p.bio.trim().is_empty()))>
                <section class="mt-4" aria-labelledby="profile-card-bio">
                    <h3 id="profile-card-bio" class="text-xs font-semibold uppercase tracking-wider text-white/60">{move || crate::i18n::t("About me")}</h3>
                    <p class="mt-1 max-h-48 overflow-y-auto whitespace-pre-wrap break-words text-sm">{move || profile.with(|p| p.as_ref().map(|p| p.bio.clone()))}</p>
                </section>
            </Show>
            <div class="mt-6 flex flex-wrap justify-end gap-2">
                <Show when=move || conversation().is_some()>
                    <button type="button" class="rounded bg-brand px-4 py-2 text-sm font-semibold" on:click=move |_| {
                        if let Some(id) = conversation() {
                            close();
                            super::friends::open(ui, id);
                        }
                    }>{move || crate::i18n::t("Send message")}</button>
                </Show>
                <Show when=move || own()>
                    <button type="button" class="rounded bg-white/10 px-4 py-2 text-sm" on:click=move |_| {
                        close();
                        ui.page.set("profile");
                    }>{move || crate::i18n::t("Edit profile")}</button>
                </Show>
                <button type="button" class="rounded bg-white/10 px-4 py-2 text-sm" aria-haspopup="menu" on:click=move |_| {
                    let opener = ui.members.opener.get_value();
                    refocus.set_value(false);
                    close();
                    if let Some(opener) = opener.filter(|o| o.is_connected()) {
                        super::context_menu::open_for(ui, opener.unchecked_into());
                    }
                }>{move || crate::i18n::t("More actions…")}</button>
            </div>
        </dialog>
    }
}
