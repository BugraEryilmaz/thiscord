//! Guild member list: a right-side panel on wide layouts, a drawer on phones and
//! tablets, and a profile card for each member. Data arrives on the chat socket.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_frontend::{
    appearance::Rgb,
    member_list::{Directory, Identity, PANEL_KEY, Section, apply_presence, sections, status_text},
};
use thiscord_shared::{
    AccountId, AvatarId, ChannelId, GuildId,
    chat::{
        CHAT_PATH, ChatRequest, ChatResponse, GuildMember, MemberActivity, MemberProfile,
        MemberRole,
    },
    friends::Relationship,
    permissions::RoleColor,
};
use wasm_bindgen::JsCast;

/// Layouts narrower than this show the list as an overlay drawer; matches the
/// `max-width: 1023px` media query in style.css (both use the layout viewport).
const DRAWER_BELOW: f64 = 1024.0;

#[derive(Clone, Copy)]
pub(super) struct State {
    /// The latest authorized list for `guild`, as delivered by the socket.
    list: RwSignal<Vec<GuildMember>>,
    /// Guild names and colours from `list`; unchanged by presence updates.
    directory: Memo<Directory>,
    guild: RwSignal<Option<GuildId>>,
    /// Member whose nickname dialog is open (see `nickname::Host`).
    pub(super) nickname: RwSignal<Option<(GuildId, AccountId)>>,
    /// Wide layouts: side panel, persisted on this device.
    panel: RwSignal<bool>,
    /// Narrow layouts: temporary drawer, closed when the shown channel or the
    /// phone content view changes (see `Panel`).
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
        let list = RwSignal::new(vec![]);
        Self {
            list,
            directory: Memo::new(move |_| list.with(|l| Directory::new(l))),
            guild: RwSignal::new(None),
            nickname: RwSignal::new(None),
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
    pub(super) fn presence(self, online: &[AccountId], activities: &[MemberActivity]) {
        self.list
            .update(|list| apply_presence(list, online, activities));
    }
    /// Current status line of a listed member, for tooltips elsewhere.
    pub(super) fn status(self, guild: GuildId, id: AccountId) -> Option<String> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.list.with(|list| {
            list.iter()
                .find(|m| m.account_id == id)
                .and_then(status_line)
        })
    }
    /// Highest-ranked role of a listed member, for the profile card.
    fn role(self, guild: GuildId, id: AccountId) -> Option<MemberRole> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.list.with(|list| {
            list.iter()
                .find(|m| m.account_id == id)
                .and_then(|m| m.role.clone())
        })
    }
    /// A listed member's name and colour in this guild (tracked). Rows keep
    /// their own snapshot, so nickname and role edits apply without a reload.
    pub(super) fn identity(self, guild: GuildId, id: AccountId) -> Option<Identity> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.directory.with(|d| d.get(id).cloned())
    }
    /// The listed member a `@username` mention refers to (tracked).
    pub(super) fn mention(self, guild: GuildId, username: &str) -> Option<Identity> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.directory.with(|d| d.username(username).cloned())
    }
    /// Live activity of a listed member, for the profile card.
    fn activity(self, guild: GuildId, id: AccountId) -> Option<String> {
        if self.guild.get() != Some(guild) {
            return None;
        }
        self.list.with(|list| {
            list.iter()
                .find(|m| m.account_id == id)
                .and_then(|m| m.activity.as_ref())
                .map(super::activity::label)
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

/// CSS colour of a member name: the role colour, adjusted to stay readable on
/// the current theme (light, dark or custom). Tracks theme changes.
pub(super) fn name_color(color: Option<RoleColor>) -> Option<String> {
    let (r, g, b) = color?.rgb();
    Some(crate::theme::current().readable_name(Rgb(r, g, b)).hex())
}

/// Text and background colours of a mention chip for a coloured member.
pub(super) fn mention_colors(color: Option<RoleColor>) -> Option<(String, String)> {
    let (r, g, b) = color?.rgb();
    let (text, background) = crate::theme::current().mention_chip(Rgb(r, g, b));
    Some((text.hex(), background.hex()))
}

/// A role tag: a dot in the role's own colour (decorative) and its name.
#[component]
pub(super) fn RoleBadge(role: MemberRole) -> impl IntoView {
    let title = role.name.clone();
    view! {
        <span class="member-badge member-role-badge" title=title>
            {role.color.map(|color| view! {<span class="role-dot" style:background-color=color.hex() aria-hidden="true"></span>})}
            <span class="truncate">{role.name}</span>
        </span>
    }
}

/// Header button showing or hiding the member list.
#[component]
pub(super) fn Toggle(ui: Ui) -> impl IntoView {
    view! {
        <button type="button" class="icon-button shrink-0" aria-controls="member-list"
            aria-expanded=move || ui.members.open().to_string()
            aria-label=move || crate::i18n::t("Member list") data-tip=move || crate::i18n::t("Member list")
            on:click=move |_| ui.members.toggle()><Icon kind=Glyph::Users/></button>
    }
}

/// Key of a rendered row; any displayed change re-creates that row only.
type RowKey = (
    AccountId,
    String,
    Option<AvatarId>,
    String,
    Option<MemberRole>,
    Option<RoleColor>,
    bool,
    bool,
    bool,
);
fn row_key(member: &GuildMember) -> RowKey {
    (
        member.account_id,
        member.display_name().to_owned(),
        member.avatar_id,
        status_line(member).unwrap_or_default(),
        member.role.clone(),
        member.color,
        member.online,
        member.owner,
        member.bot,
    )
}

/// A member's live activity, otherwise their custom status.
fn status_line(member: &GuildMember) -> Option<String> {
    member
        .activity
        .as_ref()
        .map(super::activity::label)
        .or_else(|| member.custom_status.as_ref().map(status_text))
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
    let status = status_line(&member);
    let own = move || ui.account.with(|a| a.as_ref().is_some_and(|a| a.id == id));
    let online = member.online;
    let color = member.color;
    let badge = member.role.clone();
    let role = member.role.as_ref().map(|r| r.name.clone());
    let label = {
        let name = name.clone();
        let role = role.clone();
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
            if let Some(role) = &role {
                label = format!("{label}, {role}");
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
                        <span class="truncate text-sm font-medium" style:color=move || name_color(color)>{name}</span>
                        {badge.map(|role| view! {<RoleBadge role/>})}
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
    title: Signal<String>,
    heading: String,
    members: Memo<Vec<GuildMember>>,
    roving: super::keyboard::Roving<thiscord_shared::AccountId>,
) -> impl IntoView {
    view! {
        <Show when=move || members.with(|m| !m.is_empty())>
            <section aria-labelledby=heading.clone()>
                <h3 id=heading.clone() class="member-group-heading">
                    {move || format!("{} — {}", title.get(), members.with(Vec::len))}
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
    // The panel outlives channel switches, so close the temporary drawer and any
    // profile card on navigation, including the phone back button.
    Effect::new(move |previous: Option<(Option<ChannelId>, bool)>| {
        let shown = (
            ui.selected_channel.with(|c| c.as_ref().map(|c| c.id)),
            ui.mobile_content.get(),
        );
        if previous.is_some_and(|previous| previous != shown) {
            ui.members.drawer.set(false);
            ui.members.profile.set(None);
        }
        shown
    });
    let list = Memo::new(move |_| {
        if ui.members.guild.get() == Some(guild) {
            ui.members.list.get()
        } else {
            vec![]
        }
    });
    // Role groups (highest rank first), then Online without roles, then Offline.
    let groups = Memo::new(move |_| {
        list.with(|l| {
            sections(l)
                .into_iter()
                .map(|(section, members)| (section, members.into_iter().cloned().collect()))
                .collect::<Vec<(Section, Vec<GuildMember>)>>()
        })
    });
    let keys =
        Memo::new(move |_| groups.with(|g| g.iter().map(|(s, _)| s.key()).collect::<Vec<_>>()));
    let roving = super::keyboard::Roving::new(
        move || {
            groups.with(|g| {
                g.iter()
                    .flat_map(|(_, m)| m.iter().map(|m| m.account_id))
                    .collect()
            })
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
                <h2 class="pane-title min-w-0 flex-1 truncate">{move || crate::i18n::t("Members")}</h2>
                <button node_ref=close_button type="button" class="icon-button member-drawer-close" aria-label=move || crate::i18n::t("Close member list")
                    on:click=move |_| close()><Icon kind=Glyph::Close/></button>
            </header>
            <div class="member-panel-scroll" data-rove-list on:keydown=move |e| super::keyboard::rove(&e)>
                <Show when=move || list.with(Vec::is_empty)>
                    <p class="px-2 py-4 text-sm text-white/60" role="status">{move ||
                        if ui.socket_status.get() == super::chat::ConnectionStatus::Connected { crate::i18n::t("Loading members…") }
                        else { ui.socket_status.get().label() }}</p>
                </Show>
                <For each=move || keys.get() key=|key| key.clone() children=move |key| {
                    let find = {
                        let key = key.clone();
                        move |g: &Vec<(Section, Vec<GuildMember>)>| g.iter().position(|(s, _)| s.key() == key)
                    };
                    let members = {
                        let find = find.clone();
                        Memo::new(move |_| groups.with(|g| find(g).map(|i| g[i].1.clone()).unwrap_or_default()))
                    };
                    let title = Signal::derive(move || groups.with(|g| match find(g).map(|i| &g[i].0) {
                        Some(Section::Role(role)) => role.name.clone(),
                        Some(Section::Online) => crate::i18n::t("Online").to_owned(),
                        _ => crate::i18n::t("Offline").to_owned(),
                    }));
                    view! {<Group ui guild title heading=format!("member-group-{key}") members roving/>}
                }/>
            </div>
        </aside>
    }
}

/// Profile card for a member selected in the list. Mounted once per server view.
#[component]
pub(super) fn ProfileHost(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let profile = RwSignal::new(None::<MemberProfile>);
    let error = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    // Leaving the server unmounts this host; never reopen a card there.
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
    let card_role = move || {
        ui.members
            .profile
            .get()
            .and_then(|(guild, id)| ui.members.role(guild, id))
    };
    let own = move || {
        ui.members
            .profile
            .get()
            .is_some_and(|(_, id)| ui.account.with(|a| a.as_ref().is_some_and(|a| a.id == id)))
    };
    // The guild name (nickname when set) leads; the profile name follows it.
    let heading = move || {
        profile.with(|p| {
            p.as_ref()
                .map(|p| p.nickname.clone().unwrap_or_else(|| p.display_name.clone()))
        })
    };
    let color = move || {
        ui.members
            .profile
            .get()
            .and_then(|(guild, id)| ui.members.identity(guild, id))
            .and_then(|i| name_color(i.color))
    };
    let can_rename = move || {
        own()
            && ui
                .server
                .with(|s| s.as_ref().is_some_and(|s| s.can_change_nickname))
    };
    view! {
        <dialog node_ref=dialog class="context-dialog profile-card" aria-labelledby="profile-card-name"
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, false) on:close=move |_| closed()>
            <div class="flex items-start gap-4">
                <crate::avatar::Avatar class="size-16 text-2xl"
                    name=Signal::derive(move || heading().unwrap_or_default())
                    avatar_id=Signal::derive(move || profile.with(|p| p.as_ref().and_then(|p| p.avatar_id)))/>
                <div class="min-w-0 flex-1">
                    <h2 id="profile-card-name" class="break-words text-xl font-semibold" style:color=color>{heading}</h2>
                    <p class="break-all text-sm text-white/60">{move || profile.with(|p| p.as_ref().map(|p| match &p.nickname {
                        Some(_) => format!("{} · @{}", p.display_name, p.username),
                        None => format!("@{}", p.username),
                    }))}</p>
                    <Show when=move || profile.with(|p| p.as_ref().is_some_and(|p| p.owner || p.bot)) || card_role().is_some()>
                        <p class="mt-1 flex flex-wrap gap-1">
                            {move || card_role().map(|role| view! {<RoleBadge role/>})}
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
            {move || ui.members.profile.get().and_then(|(guild, id)| ui.members.activity(guild, id)).map(|activity| view! {
                <section class="mt-4" aria-labelledby="profile-card-activity">
                    <h3 id="profile-card-activity" class="text-xs font-semibold uppercase tracking-wider text-white/60">{move || crate::i18n::t("Activity")}</h3>
                    <p class="mt-1 break-words text-sm">{activity}</p>
                </section>
            })}
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
                <Show when=can_rename>
                    <button type="button" class="rounded bg-white/10 px-4 py-2 text-sm" aria-haspopup="dialog" on:click=move |_| {
                        // Return focus to the list row, not this closing card.
                        let target = ui.members.profile.get_untracked();
                        refocus.set_value(false);
                        close();
                        if let Some(opener) = ui.members.opener.get_value().filter(|o| o.is_connected()) {
                            let _ = opener.focus();
                        }
                        ui.members.nickname.set(target);
                    }>{move || crate::i18n::t("Change nickname")}</button>
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
