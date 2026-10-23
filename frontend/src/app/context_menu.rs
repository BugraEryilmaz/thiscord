use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{
    AccountId, ChannelId, GuildId, audio::SpeakerVolumeTarget, chat::*, permissions::*,
};
use wasm_bindgen::JsCast;

#[derive(Clone)]
enum Action {
    Bots(super::voice_bots::Target),
    Copy(String),
    Page(GuildId, &'static str, Option<AccountId>),
    ChannelPermissions(GuildId, ChannelId),
    Read(GuildId, ChannelId),
    Preference(ChannelId, bool),
    Notifications(thiscord_shared::notifications::Target, String),
    Snooze(
        thiscord_shared::notifications::Target,
        thiscord_shared::notifications::Snooze,
    ),
    Join(Guild, Channel),
    Disconnect,
    Friend(thiscord_shared::friends::Request),
    Unfriend(AccountId),
    Block(AccountId),
    ReportUser(AccountId),
    Change(GuildId, i64, GuildChange, String),
}
#[derive(Clone)]
enum Item {
    Command {
        label: String,
        action: Action,
        danger: bool,
    },
    Mute(thiscord_shared::notifications::Target),
}
impl Item {
    fn new(label: impl Into<String>, action: Action) -> Self {
        Self::Command {
            label: label.into(),
            action,
            danger: false,
        }
    }
    fn danger(mut self) -> Self {
        if let Self::Command { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }
}
#[derive(Clone)]
pub(super) struct Menu {
    title: String,
    items: Vec<Item>,
    note: String,
    x: f64,
    y: f64,
    volume: Option<(GuildId, ChannelId, AccountId)>,
    member: Option<AccountId>,
}
#[derive(Clone, Copy)]
struct Target {
    guild: GuildId,
    channel: Option<ChannelId>,
    member: Option<AccountId>,
}
fn target(element: &web_sys::Element) -> Option<Target> {
    Some(Target {
        guild: element.get_attribute("data-context-server")?.parse().ok()?,
        channel: element
            .get_attribute("data-context-channel")
            .and_then(|v| v.parse().ok()),
        member: element
            .get_attribute("data-context-member")
            .and_then(|v| v.parse().ok()),
    })
}
fn element(target: Option<web_sys::EventTarget>) -> Option<web_sys::Element> {
    target?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest("[data-context-server]")
        .ok()?
}
fn close(ui: Ui) {
    ui.context_generation.update(|v| *v = v.wrapping_add(1));
    ui.context_menu.set(None);
}
fn preference_key(account: AccountId) -> String {
    format!("thiscord.sidebar.v1.{account}")
}

#[component]
pub(super) fn ParticipantVolume(
    ui: Ui,
    guild: GuildId,
    channel: ChannelId,
    member: AccountId,
) -> impl IntoView {
    let target = SpeakerVolumeTarget {
        guild_id: guild,
        account_id: member,
        shared_audio: false,
    };
    let connected = move || ui.voice.get().connected && ui.voice.get().channel_id == Some(channel);
    let streams = Memo::new(move |_| {
        ui.audio_status
            .get()
            .map(|s| {
                s.streams
                    .into_iter()
                    .filter(|s| s.target == Some(target))
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    view! {<section class="context-volume" role="group" aria-label=move || crate::i18n::t("Participant volume")>
        <Show when=move ||connected() && !streams.get().is_empty() fallback=move ||view!{<label class="volume-placeholder">{move || crate::i18n::t("Voice volume")}<input type="range" min="0" max="200" value="100" disabled aria-label=move || crate::i18n::t("Voice volume")/></label><p class="volume-hint">{move ||if connected(){crate::i18n::t("Waiting for voice audio…")}else{crate::i18n::t("Join this call to adjust volume.")}}</p>}>
            <For each=move ||streams.get() key=|id|id.clone() children=move |id|{
                let stream=ui.audio_status.get_untracked().and_then(|s|s.streams.into_iter().find(|s|s.id==id && s.target==Some(target)));
                stream.map(|stream|view!{<super::audio::StreamVolume ui=ui stream=stream compact=true/>})
            }/>
        </Show>
        <For each=move ||{ui.audio_status.get().map(|s|s.streams).unwrap_or_default().into_iter().filter(|s| connected() && s.target.is_some_and(|t|t.guild_id==guild && t.account_id==member && t.shared_audio)).collect::<Vec<_>>()} key=|s|(s.id.clone(),s.target) children=move |stream|view!{<super::audio::StreamVolume ui=ui stream=stream compact=true/>}/>
        <p class="volume-hint">{move || crate::i18n::t("Only changes what you hear. 0% mutes.")}</p>
    </section>}
}
fn save_preference(ui: Ui, channel: ChannelId, pin: bool) {
    ui.sidebar_preferences.update(|p| p.toggle(channel, pin));
    if let Some(account) = ui.account.get_untracked() {
        let saved = window().local_storage().ok().flatten().and_then(|s| {
            let value = serde_json::to_string(&ui.sidebar_preferences.get_untracked()).ok()?;
            s.set_item(&preference_key(account.id), &value).ok()
        });
        if saved.is_none() {
            ui.status
                .set("Preference applied for this session; device storage is unavailable".into());
        }
    }
}

fn open(ui: Ui, element: web_sys::Element, x: f64, y: f64) {
    let Some(target) = target(&element) else {
        close(ui);
        return;
    };
    if ui.busy.get_untracked() {
        close(ui);
        return;
    }
    close(ui);
    let generation = ui.context_generation.get_untracked();
    let token = ui.token.get_untracked();
    let rect = element.get_bounding_client_rect();
    let (x, y) = if x == 0.0 && y == 0.0 {
        (rect.left(), rect.bottom())
    } else {
        (x, y)
    };
    ui.context_menu.set(Some(Menu {
        title: "Loading…".into(),
        items: vec![],
        note: String::new(),
        x,
        y,
        member: target.member,
        volume: target
            .member
            .filter(|id| ui.account.get_untracked().is_some_and(|a| a.id != *id))
            .zip(target.channel)
            .map(|(member, channel)| (target.guild, channel, member)),
    }));
    leptos::task::spawn_local(async move {
        let result = items(ui, target).await;
        if ui.context_generation.get_untracked() != generation || ui.token.get_untracked() != token
        {
            return;
        }
        ui.context_menu.update(|menu| {
            if let Some(menu) = menu {
                match result {
                    Ok((title, items)) => {
                        menu.title = title;
                        menu.items = items;
                    }
                    Err(error) => {
                        menu.title = "Unavailable".into();
                        menu.note = error;
                    }
                }
            }
        });
    });
}
async fn items(ui: Ui, target: Target) -> Result<(String, Vec<Item>), String> {
    let PermissionResponse::Home { home } = super::servers::request(
        ui,
        PermissionRequest::ViewGuild {
            guild_id: target.guild,
        },
    )
    .await?
    else {
        return Err("Unexpected server response".into());
    };
    let guild = home.guild;
    let mut items = vec![];
    if let Some(member) = target.member {
        let channel = target
            .channel
            .ok_or(crate::i18n::t("Voice channel unavailable"))?;
        if !home.channels.iter().any(|c| c.id == channel) {
            return Err("Channel is no longer accessible".into());
        }
        let label = ui
            .voice_roster
            .get_untracked()
            .iter()
            .find(|r| r.channel_id == channel)
            .and_then(|r| r.members.iter().find(|m| m.account_id == member))
            .map(|m| m.display_name().to_owned())
            .or_else(|| {
                ui.voice
                    .get_untracked()
                    .participants
                    .iter()
                    .find(|m| m.account_id == member)
                    .map(|m| m.display_name().to_owned())
            })
            .unwrap_or_else(|| "Voice participant".into());
        let own = ui.account.get_untracked().is_some_and(|a| a.id == member);
        if is_bot(ui, member) {
            if matches!(super::servers::request(ui, PermissionRequest::Instance {}).await, Ok(PermissionResponse::Instance { access }) if access.role == InstanceRole::Owner)
            {
                items.push(Item::new(
                    crate::i18n::t("Test bot audio settings…"),
                    Action::Bots(super::voice_bots::Target {
                        guild: guild.id,
                        channel,
                        name: home
                            .channels
                            .iter()
                            .find(|c| c.id == channel)
                            .map(|c| c.name.clone())
                            .unwrap_or_default(),
                    }),
                ));
            }
            return Ok((label, items));
        }
        if own {
            items.push(Item::new(
                crate::i18n::t("Your audio settings"),
                Action::Page(guild.id, "audio", None),
            ));
        } else {
            if home.can_manage_roles
                && let Ok(PermissionResponse::State { state }) =
                    super::servers::request(ui, PermissionRequest::Inspect { guild_id: guild.id })
                        .await
                && ui
                    .account
                    .get_untracked()
                    .is_some_and(|a| thiscord_frontend::sidebar::lower_member(&state, a.id, member))
            {
                items.push(Item::new(
                    crate::i18n::t("Change roles…"),
                    Action::Page(guild.id, "permissions", Some(member)),
                ));
            }
            if home.can_moderate
                && let Ok(PermissionResponse::Moderation { state }) = super::servers::request(
                    ui,
                    PermissionRequest::InspectModeration { guild_id: guild.id },
                )
                .await
                && let Some(m) = state.members.iter().find(|m| m.account_id == member)
            {
                for (permission, label, change) in [
                    (
                        Permission::MoveMembers,
                        crate::i18n::t("Disconnect from voice"),
                        GuildChange::DisconnectVoice { account_id: member },
                    ),
                    (
                        Permission::KickMembers,
                        crate::i18n::t("Kick from server"),
                        GuildChange::RemoveMember { account_id: member },
                    ),
                    (
                        Permission::BanMembers,
                        crate::i18n::t("Ban from server"),
                        GuildChange::BanMember { account_id: member },
                    ),
                ] {
                    if m.actions.contains(&permission) {
                        items.push(
                            Item::new(
                                label,
                                Action::Change(
                                    guild.id,
                                    state.guild.revision,
                                    change,
                                    thiscord_frontend::localized_format!(
                                        crate::i18n::language(),
                                        "{label}: {}?",
                                        m.display_name(),
                                        label = label
                                    ),
                                ),
                            )
                            .danger(),
                        );
                    }
                }
                if !m.actions.is_empty() {
                    items.push(Item::new(
                        crate::i18n::t("Timeout / moderation…"),
                        Action::Page(guild.id, "moderation", Some(member)),
                    ));
                }
            }
        }
        items.push(Item::new(
            crate::i18n::t("Copy user ID"),
            Action::Copy(member.to_string()),
        ));
        return Ok((label, items));
    }
    if let Some(id) = target.channel {
        let channel = home
            .channels
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(crate::i18n::t("Channel is no longer accessible"))?;
        let permissions = if let Some(account) = ui.account.get_untracked() {
            match super::servers::request(
                ui,
                PermissionRequest::Preview {
                    guild_id: guild.id,
                    account_id: account.id,
                    channel_id: Some(id),
                },
            )
            .await
            {
                Ok(PermissionResponse::Effective { permissions, .. }) => permissions,
                _ => Permissions::new(),
            }
        } else {
            Permissions::new()
        };
        if channel.kind == ChannelKind::Text && permissions.contains(&Permission::ReadHistory) {
            items.push(Item::new(
                crate::i18n::t("Mark as read"),
                Action::Read(guild.id, id),
            ));
        }
        if channel.kind == ChannelKind::Voice {
            if permissions.contains(&Permission::JoinVoice)
                && permissions.contains(&Permission::Speak)
                && matches!(super::servers::request(ui, PermissionRequest::Instance {}).await, Ok(PermissionResponse::Instance { access }) if access.role == InstanceRole::Owner)
            {
                items.push(Item::new(
                    crate::i18n::t("Voice test bots…"),
                    Action::Bots(super::voice_bots::Target {
                        guild: guild.id,
                        channel: id,
                        name: channel.name.clone(),
                    }),
                ));
            }
            if ui.voice.get_untracked().channel_id == Some(id) {
                items.push(Item::new(crate::i18n::t("Disconnect"), Action::Disconnect));
            } else if permissions.contains(&Permission::JoinVoice) {
                items.push(Item::new(
                    crate::i18n::t("Join voice"),
                    Action::Join(guild.clone(), channel.clone()),
                ));
            }
        }
        let preferences = ui.sidebar_preferences.get_untracked();
        let notification_target = thiscord_shared::notifications::Target::Channel {
            guild_id: guild.id,
            channel_id: id,
        };
        items.push(Item::new(
            crate::i18n::t("Notification settings…"),
            Action::Notifications(notification_target, channel.name.clone()),
        ));
        items.push(Item::Mute(notification_target));
        items.push(Item::new(
            if preferences.pinned.contains(&id) {
                crate::i18n::t("Unpin from top")
            } else {
                crate::i18n::t("Pin to top")
            },
            Action::Preference(id, true),
        ));
        items.push(Item::new(
            crate::i18n::t("Copy channel ID"),
            Action::Copy(id.to_string()),
        ));
        if home.can_moderate {
            items.push(Item::new(
                crate::i18n::t("Server moderation…"),
                Action::Page(guild.id, "moderation", None),
            ));
        }
        if home.can_manage_roles {
            items.push(Item::new(
                crate::i18n::t("Channel permissions…"),
                Action::ChannelPermissions(guild.id, id),
            ));
        }
        if home.can_manage_channels {
            items.push(
                Item::new(
                    crate::i18n::t("Delete channel…"),
                    Action::Change(
                        guild.id,
                        guild.revision,
                        GuildChange::DeleteChannel { channel_id: id },
                        thiscord_frontend::localized_format!(
                            crate::i18n::language(),
                            "Delete #{}? This removes the channel and its messages.",
                            channel.name
                        ),
                    ),
                )
                .danger(),
            );
        }
        return Ok((channel.name, items));
    }
    items.push(Item::new(
        crate::i18n::t("Copy server ID"),
        Action::Copy(guild.id.to_string()),
    ));
    items.push(Item::new(
        crate::i18n::t("Open server"),
        Action::Page(guild.id, "servers", None),
    ));
    if home.can_manage_roles {
        items.push(Item::new(
            crate::i18n::t("Server settings…"),
            Action::Page(guild.id, "permissions", None),
        ));
    }
    if home.can_manage_bots {
        items.push(Item::new(
            crate::i18n::t("Bots"),
            Action::Page(guild.id, "server_bots", None),
        ));
    }
    if home.can_moderate {
        items.push(Item::new(
            crate::i18n::t("Server moderation…"),
            Action::Page(guild.id, "moderation", None),
        ));
    }
    if ui
        .account
        .get_untracked()
        .is_some_and(|a| a.id != guild.owner)
    {
        items.push(
            Item::new(
                crate::i18n::t("Leave server…"),
                Action::Change(
                    guild.id,
                    guild.revision,
                    GuildChange::Leave {},
                    thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "Leave {}? You will need its join details to return.",
                        guild.name
                    ),
                ),
            )
            .danger(),
        );
    }
    Ok((guild.name, items))
}

fn execute(ui: Ui, action: Action) {
    execute_reason(ui, action, String::new());
}
fn execute_reason(ui: Ui, action: Action, reason: String) {
    match action {
        Action::Block(id) => super::safety::block(ui, id),
        Action::ReportUser(account_id) => {
            ui.friends
                .report_target
                .set(Some(thiscord_shared::safety::ReportTarget::User {
                    account_id,
                }))
        }
        Action::Bots(target) => ui.bot_target.set(Some(target)),
        Action::Friend(command) => super::friends::action(ui, command),
        Action::Unfriend(account_id) => {
            super::friends::action(ui, thiscord_shared::friends::Request::Remove { account_id })
        }
        Action::Notifications(target, title) => super::notifications::edit(ui, target, title),
        Action::Snooze(target, duration) => super::notifications::snooze(ui, target, duration),
        Action::Preference(channel, pin) => save_preference(ui, channel, pin),
        Action::Join(guild, channel) => super::audio::join(ui, guild, channel),
        Action::Disconnect => super::audio::leave(ui),
        Action::Copy(value) => leptos::task::spawn_local(async move {
            let result = wasm_bindgen_futures::JsFuture::from(
                window().navigator().clipboard().write_text(&value),
            )
            .await;
            ui.status.set(if result.is_ok() {
                "ID copied".into()
            } else {
                "Could not copy ID".into()
            });
        }),
        Action::Page(guild, page, member) => leptos::task::spawn_local(async move {
            match super::servers::select(ui, guild).await {
                Ok(false) => (),
                Ok(true) => {
                    ui.role_target.set(member.map(|m| (guild, m)));
                    ui.permission_channel.set(None);
                    ui.moderation_target.set(member);
                    ui.page.set(page);
                }
                Err(error) => ui.status.set(error),
            }
        }),
        Action::ChannelPermissions(guild, channel) => leptos::task::spawn_local(async move {
            match super::servers::select(ui, guild).await {
                Ok(false) => (),
                Ok(true) => {
                    ui.role_target.set(None);
                    ui.permission_channel.set(Some((guild, channel)));
                    ui.page.set("permissions");
                }
                Err(error) => ui.status.set(error),
            }
        }),
        Action::Read(guild_id, channel_id) => leptos::task::spawn_local(async move {
            let token = ui.token.get_untracked();
            let result = crate::account_client::api_request::<_, ChatResponse>(
                CHAT_PATH,
                &ChatRequest::Read {
                    guild_id,
                    channel_id,
                    through: i64::MAX,
                },
                token.as_deref(),
            )
            .await;
            if ui.token.get_untracked() != token {
                return;
            }
            match result {
                Ok(_) => {
                    ui.unread
                        .update(|rows| rows.retain(|r| r.channel_id != channel_id));
                }
                Err(error) => ui.status.set(error),
            }
        }),
        Action::Change(guild_id, revision, change, _) => {
            if ui.busy.get_untracked() {
                return;
            }
            ui.busy.set(true);
            leptos::task::spawn_local(async move {
                let selection = ui.navigation.get_untracked().current();
                let token = ui.token.get_untracked();
                let result = super::servers::request(
                    ui,
                    if matches!(
                        change,
                        GuildChange::RemoveMember { .. }
                            | GuildChange::BanMember { .. }
                            | GuildChange::DisconnectVoice { .. }
                    ) {
                        PermissionRequest::Moderate {
                            guild_id,
                            revision,
                            change,
                            reason,
                        }
                    } else {
                        PermissionRequest::Change {
                            guild_id,
                            revision,
                            change,
                        }
                    },
                )
                .await;
                if !super::servers::current(ui, selection) || ui.token.get_untracked() != token {
                    ui.busy.set(false);
                    return;
                }
                ui.status.set(match result {
                    Ok(_) => "Action completed".into(),
                    Err(error) => {
                        thiscord_frontend::localized_format!(
                            crate::i18n::language(),
                            "{error}. Reopen the menu to refresh permissions before retrying.",
                            error = crate::i18n::message(error)
                        )
                    }
                });
                super::servers::refresh(ui).await;
                ui.busy.set(false);
            });
        }
    }
}

fn conversation_menu(ui: Ui, event_target: Option<web_sys::EventTarget>, x: f64, y: f64) -> bool {
    let Some(element) = event_target
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|e| e.closest("[data-context-conversation]").ok().flatten())
    else {
        return false;
    };
    let Some(conversation_id) = element
        .get_attribute("data-context-conversation")
        .and_then(|id| id.parse().ok())
    else {
        return false;
    };
    let target = thiscord_shared::notifications::Target::Conversation { conversation_id };
    let friend = ui
        .friends
        .snapshot
        .get_untracked()
        .friends
        .into_iter()
        .find(|f| f.conversation_id == conversation_id);
    let Some(friend) = friend else {
        return false;
    };
    let title = friend.person.display_name;
    let mut items = vec![Item::new(
        crate::i18n::t("Notification settings…"),
        Action::Notifications(target, title.clone()),
    )];
    items.push(Item::Mute(target));
    if friend.relationship != thiscord_shared::friends::Relationship::Friend {
        items.clear();
    }
    let (x, y) = menu_position(&element, x, y);
    close(ui);
    ui.context_menu.set(Some(Menu {
        title,
        items,
        note: String::new(),
        x,
        y,
        volume: None,
        member: Some(friend.person.id),
    }));
    true
}

fn menu_position(element: &web_sys::Element, x: f64, y: f64) -> (f64, f64) {
    if x == 0.0 && y == 0.0 {
        let rect = element.get_bounding_client_rect();
        (rect.left(), rect.bottom())
    } else {
        (x, y)
    }
}

fn user_menu(ui: Ui, event_target: Option<web_sys::EventTarget>, x: f64, y: f64) -> bool {
    let Some(element) = event_target
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|e| e.closest("[data-context-user]").ok().flatten())
    else {
        return false;
    };
    let Some(member) = element
        .get_attribute("data-context-user")
        .and_then(|id| id.parse::<AccountId>().ok())
    else {
        return false;
    };
    let (x, y) = menu_position(&element, x, y);
    close(ui);
    ui.context_menu.set(Some(Menu {
        title: element
            .get_attribute("data-context-name")
            .unwrap_or_else(|| "User".into()),
        items: vec![Item::new(
            crate::i18n::t("Copy user ID"),
            Action::Copy(member.to_string()),
        )],
        note: String::new(),
        x,
        y,
        volume: None,
        member: Some(member),
    }));
    true
}

#[component]
fn FriendActions(ui: Ui, member: AccountId, run: Callback<Action>) -> impl IntoView {
    use thiscord_frontend::sidebar::{FriendAction, friend_actions};
    view! { {move || {
        if is_bot(ui, member) { return Vec::new(); }
        let account = ui.account.get();
        let own = account.as_ref().is_none_or(|a|a.id == member);
        let relationship = ui.friends.snapshot.with(|s|s.friends.iter().find(|f|f.person.id==member).map(|f|f.relationship));
        friend_actions(own, relationship).iter().copied().map(|action| {
            let disabled = !ui.friends.connected.get() || (action==FriendAction::Add && !account.as_ref().is_some_and(|a|a.email_verified));
            view! {<button role="menuitem" class="context-item" class:context-danger=action==FriendAction::Unfriend disabled=disabled on:click=move |_|run.run(if action==FriendAction::Unfriend {Action::Unfriend(member)} else {Action::Friend(action.request(member))})>{move ||crate::i18n::t(action.label())}</button>}
        }).collect_view()
    }}
    <Show when=move ||!is_bot(ui,member) && ui.account.get().is_some_and(|a|a.id!=member)>
        <button role="menuitem" class="context-item context-danger" on:click=move |_|run.run(Action::Block(member))>{move ||crate::i18n::t("Block account")}</button>
        <button role="menuitem" class="context-item" on:click=move |_|run.run(Action::ReportUser(member))>{move ||crate::i18n::t("Report account")}</button>
    </Show> }
}

fn is_bot(ui: Ui, member: AccountId) -> bool {
    ui.voice.with(|v| {
        v.participants
            .iter()
            .any(|p| p.account_id == member && p.test_bot)
    }) || ui.voice_roster.with(|rooms| {
        rooms.iter().any(|r| {
            r.members
                .iter()
                .any(|p| p.account_id == member && p.test_bot)
        })
    })
}

#[component]
fn MuteSubmenu(
    target: thiscord_shared::notifications::Target,
    open: RwSignal<bool>,
    parent_position: RwSignal<(f64, f64)>,
    run: Callback<Action>,
) -> impl IntoView {
    let group = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let submenu = NodeRef::<leptos::html::Div>::new();
    let position = RwSignal::new((8.0, 8.0));
    let focus_on_open = RwSignal::new(false);
    let dismiss = move || {
        if let Some(menu) = submenu.get_untracked()
            && document()
                .active_element()
                .is_some_and(|e| menu.contains(Some(&e)))
            && let Some(button) = trigger.get_untracked()
        {
            let _ = button.focus();
        }
        open.set(false);
        focus_on_open.set(false);
    };
    Effect::new(move |_| {
        parent_position.track();
        if open.get()
            && let (Some(button), Some(menu)) = (trigger.get(), submenu.get())
        {
            let anchor = button.get_bounding_client_rect();
            let rect = menu.get_bounding_client_rect();
            let w = window()
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(800.0);
            let h = window()
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(600.0);
            // Touch the trigger so moving into the flyout never crosses a hover gap.
            let x = if anchor.right() + rect.width() <= w - 8.0 {
                anchor.right()
            } else {
                anchor.left() - rect.width()
            };
            position.set((
                x.clamp(8.0, (w - rect.width() - 8.0).max(8.0)),
                anchor.top().clamp(8.0, (h - rect.height() - 8.0).max(8.0)),
            ));
            if focus_on_open.get() {
                focus_menu_item(&menu, "Home");
                focus_on_open.set(false);
            }
        }
    });
    view! {
        <div node_ref=group class="context-submenu-group"
            on:pointerenter=move |_|open.set(true)
            on:pointerleave=move |_|dismiss()
            on:focusout=move |e| {
                let inside = e.related_target().and_then(|t|t.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|next|group.get_untracked().is_some_and(|node|node.contains(Some(&next))));
                if !inside {open.set(false);}
            }
            on:keydown=move |e| {
                let in_submenu = e.target().and_then(|t|t.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|node|submenu.get_untracked().is_some_and(|menu|menu.contains(Some(&node))));
                let key = e.key();
                if open.get_untracked() && matches!(key.as_str(), "Escape" | "ArrowLeft") {
                    e.prevent_default();
                    e.stop_propagation();
                    dismiss();
                } else if in_submenu && matches!(key.as_str(), "ArrowDown" | "ArrowUp" | "Home" | "End") {
                    e.prevent_default();
                    e.stop_propagation();
                    if let Some(menu) = submenu.get_untracked() {focus_menu_item(&menu, &key);}
                } else if !in_submenu && matches!(key.as_str(), "ArrowRight" | "ArrowDown" | "Enter" | " ") {
                    e.prevent_default();
                    e.stop_propagation();
                    focus_on_open.set(true);
                    open.set(true);
                }
            }>
            <button node_ref=trigger role="menuitem" class="context-item context-submenu-trigger"
                aria-haspopup="menu" aria-expanded=move ||open.get().to_string()
                aria-controls="context-mute-options"
                on:click=move |_|{focus_on_open.set(true);open.set(true);}>
                <span>{move || crate::i18n::t("Mute")}</span><span aria-hidden="true">"›"</span>
            </button>
            <Show when=move ||open.get()>
                <div node_ref=submenu id="context-mute-options" class="context-submenu" role="menu" aria-label=move || crate::i18n::t("Mute duration")
                    style:left=move ||format!("{}px",position.get().0)
                    style:top=move ||format!("{}px",position.get().1)>
                    {thiscord_shared::notifications::Snooze::OPTIONS.into_iter().map(|(duration,label)|view! {
                        <button role="menuitem" class="context-item" on:click=move |_|run.run(Action::Snooze(target,duration))>{move ||crate::i18n::snooze_label(duration,label)}</button>
                    }).collect_view()}
                </div>
            </Show>
        </div>
    }
}

fn focus_menu_item(menu: &web_sys::Element, key: &str) {
    // Only traverse this menu's level; flyout entries have their own navigation.
    let Ok(nodes) = menu.query_selector_all(
        ":scope > button[role=menuitem]:not(:disabled), :scope > .context-submenu-group > button[role=menuitem]:not(:disabled)",
    ) else {
        return;
    };
    let buttons = (0..nodes.length())
        .filter_map(|i| nodes.item(i)?.dyn_into::<web_sys::HtmlElement>().ok())
        .collect::<Vec<_>>();
    if buttons.is_empty() {
        return;
    }
    let current = buttons
        .iter()
        .position(|b| document().active_element().as_ref() == Some(b.as_ref()));
    let index = match key {
        "Home" => 0,
        "End" => buttons.len() - 1,
        "ArrowUp" => current.map_or(buttons.len() - 1, |i| {
            (i + buttons.len() - 1) % buttons.len()
        }),
        _ => current.map_or(0, |i| (i + 1) % buttons.len()),
    };
    let _ = buttons[index].focus();
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let menu_node = NodeRef::<leptos::html::Div>::new();
    let volume = Memo::new(move |_| ui.context_menu.get().and_then(|m| m.volume));
    let position = RwSignal::new((8.0, 8.0));
    let mute_open = RwSignal::new(false);
    Effect::new(move |_| {
        ui.context_generation.track();
        mute_open.set(false);
    });
    let return_focus = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let pending = RwSignal::new(None::<Action>);
    let reason = RwSignal::new(String::new());
    let confirm = NodeRef::<leptos::html::Dialog>::new();
    let account = Memo::new(move |_| ui.account.get().map(|a| a.id));
    Effect::new(move |_| {
        close(ui);
        pending.set(None);
        if let Some(dialog) = confirm.get_untracked() {
            dialog.close();
        }
        let preferences = account
            .get()
            .and_then(|a| {
                window()
                    .local_storage()
                    .ok()
                    .flatten()?
                    .get_item(&preference_key(a))
                    .ok()?
            })
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        ui.sidebar_preferences.set(preferences);
    });
    let context = window_event_listener(leptos::ev::contextmenu, move |e| {
        e.prevent_default();
        return_focus.set_value(document().active_element().and_then(|e| e.dyn_into().ok()));
        if user_menu(ui, e.target(), e.client_x().into(), e.client_y().into())
            || conversation_menu(ui, e.target(), e.client_x().into(), e.client_y().into())
        {
            return;
        }
        if let Some(element) = element(e.target()) {
            open(ui, element, e.client_x().into(), e.client_y().into());
        } else {
            close(ui);
        }
    });
    let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|e| e.closest(".app-context-menu").ok().flatten())
            .is_some();
        if !inside {
            close(ui);
        }
    });
    let key = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "ContextMenu" || (e.key() == "F10" && e.shift_key()) {
            e.prevent_default();
            return_focus.set_value(document().active_element().and_then(|e| e.dyn_into().ok()));
            if user_menu(ui, e.target(), 0.0, 0.0) || conversation_menu(ui, e.target(), 0.0, 0.0) {
                return;
            }
            if let Some(element) = element(e.target()) {
                open(ui, element, 0.0, 0.0);
            }
        } else if ui.context_menu.get_untracked().is_some() {
            if e.key() == "Escape" {
                close(ui);
                e.prevent_default();
                if let Some(element) = return_focus.get_value() {
                    let _ = element.focus();
                }
            } else if matches!(e.key().as_str(), "ArrowDown" | "ArrowUp" | "Home" | "End")
                && !e
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|e| e.matches("input[type=range]").unwrap_or(false))
            {
                e.prevent_default();
                if let Some(menu) = menu_node.get() {
                    focus_menu_item(&menu, &e.key());
                }
            }
        }
    });
    let focus = window_event_listener(leptos::ev::focusin, move |e| {
        if ui.context_menu.get_untracked().is_some()
            && e.target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|e| e.closest(".app-context-menu").ok().flatten())
                .is_none()
        {
            close(ui);
        }
    });
    let resize = window_event_listener(leptos::ev::resize, move |_| close(ui));
    let blur = window_event_listener(leptos::ev::blur, move |_| close(ui));
    let wheel = window_event_listener(leptos::ev::wheel, move |e| {
        let inside = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|e| e.closest(".app-context-menu").ok().flatten())
            .is_some();
        if !inside {
            close(ui);
        }
    });
    on_cleanup(move || {
        context.remove();
        outside.remove();
        key.remove();
        focus.remove();
        resize.remove();
        blur.remove();
        wheel.remove();
    });
    Effect::new(move |_| {
        if let Some(menu) = ui.context_menu.get()
            && let Some(node) = menu_node.get()
        {
            let rect = node.get_bounding_client_rect();
            let w = window()
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(800.0);
            let h = window()
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(600.0);
            position.set((
                menu.x.clamp(8.0, (w - rect.width() - 8.0).max(8.0)),
                menu.y.clamp(8.0, (h - rect.height() - 8.0).max(8.0)),
            ));
            if !document()
                .active_element()
                .is_some_and(|e| node.contains(Some(&e)))
                && let Ok(Some(first)) = node.query_selector(
                    "input[type=range]:not(:disabled),button[role=menuitem]:not(:disabled)",
                )
                && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
            {
                let _ = first.focus();
            }
        }
    });
    let run = move |action: Action| {
        close(ui);
        match action {
            Action::Change(..) | Action::Unfriend(_) => {
                reason.set(String::new());
                pending.set(Some(action));
                if let Some(dialog) = confirm.get() {
                    let _ = dialog.show_modal();
                }
            }
            other => execute(ui, other),
        }
    };
    view! {
        <Show when=move ||ui.context_menu.get().is_some()>
            <div node_ref=menu_node class="app-context-menu" role="menu" aria-label=move ||ui.context_menu.get().map(|m|m.title).unwrap_or_default() style:left=move ||format!("{}px",position.get().0) style:top=move ||format!("{}px",position.get().1) on:scroll=move |_|mute_open.set(false)>
                <p class="context-heading">{move ||ui.context_menu.get().map(|m|m.title)}</p>
                {move ||ui.context_menu.get().and_then(|m|m.member).map(|member|view!{<FriendActions ui=ui member=member run=Callback::new(run)/>})}
                {move ||volume.get().map(|(guild,channel,member)|view!{<ParticipantVolume ui=ui guild=guild channel=channel member=member/>})}
                {move ||ui.context_menu.get().map(|m|m.items.into_iter().map(|item|match item {
                    Item::Command {label,action,danger} => view!{<button role="menuitem" class="context-item" class:context-danger=danger on:click=move |_|run(action.clone())>{move ||crate::i18n::t(&label).to_owned()}</button>}.into_any(),
                    Item::Mute(target) => view!{<MuteSubmenu target=target open=mute_open parent_position=position run=Callback::new(run)/>}.into_any(),
                }).collect_view())}
                <p class="px-2 text-xs text-white/60" role="status">{move ||ui.context_menu.get().map(|m|m.note)}</p>
            </div>
        </Show>
        <dialog node_ref=confirm class="context-dialog" aria-label=move || crate::i18n::t("Confirm action") on:click=move |e|super::dialog::dismiss_outside(e,confirm,ui.busy.get_untracked()) on:close=move |_|pending.set(None)>
            <h2 class="text-xl font-semibold">{move ||pending.get().and_then(|a|match a {Action::Change(_,_,_,title)=>Some(title),Action::Unfriend(_)=>Some("Remove this friend?".into()),_=>None})}</h2>
            <Show when=move ||matches!(pending.get(),Some(Action::Unfriend(_)))><p class="mt-3 text-sm text-white/60">{move || crate::i18n::t("Private messages and calls will be unavailable until you become friends again.")}</p></Show>
            <Show when=move ||matches!(pending.get(),Some(Action::Change(_,_,GuildChange::RemoveMember {..}|GuildChange::BanMember {..}|GuildChange::DisconnectVoice {..},_)))><label class="settings-field"><span>{move || crate::i18n::t("Reason (optional, up to 500 characters)")}</span><input maxlength="500" prop:value=move ||reason.get() on:input=move |e|reason.set(event_target_value(&e))/></label></Show>
            <div class="mt-6 flex justify-end gap-3"><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|if let Some(d)=confirm.get(){d.close();}>{move || crate::i18n::t("Cancel")}</button><button class="rounded bg-red-500/20 px-4 py-2 text-red-200" disabled=move ||ui.busy.get() on:click=move |_|{if let Some(action)=pending.get_untracked(){execute_reason(ui,action,reason.get_untracked());}
                if let Some(d)=confirm.get(){d.close();}}>{move || crate::i18n::t("Confirm")}</button></div>
        </dialog>

    }
}
