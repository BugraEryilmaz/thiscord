use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{
    AccountId, ChannelId, GuildId, audio::SpeakerVolumeTarget, chat::*, permissions::*,
};
use wasm_bindgen::JsCast;

#[derive(Clone)]
enum Action {
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
    Change(GuildId, i64, GuildChange, String),
}
#[derive(Clone)]
struct Item {
    label: String,
    action: Action,
    danger: bool,
    disabled: bool,
}
impl Item {
    fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action,
            danger: false,
            disabled: false,
        }
    }
    fn danger(mut self) -> Self {
        self.danger = true;
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
    view! {<section class="context-volume" role="group" aria-label="Participant volume">
        <Show when=move ||connected() && !streams.get().is_empty() fallback=move ||view!{<label class="volume-placeholder">"Voice volume"<input type="range" min="0" max="200" value="100" disabled aria-label="Voice volume"/></label><p class="volume-hint">{move ||if connected(){"Waiting for voice audio…"}else{"Join this call to adjust volume."}}</p>}>
            <For each=move ||streams.get() key=|id|id.clone() children=move |id|{
                let stream=ui.audio_status.get_untracked().and_then(|s|s.streams.into_iter().find(|s|s.id==id && s.target==Some(target)));
                stream.map(|stream|view!{<super::audio::StreamVolume ui=ui stream=stream compact=true/>})
            }/>
        </Show>
        <For each=move ||{ui.audio_status.get().map(|s|s.streams).unwrap_or_default().into_iter().filter(|s| connected() && s.target.is_some_and(|t|t.guild_id==guild && t.account_id==member && t.shared_audio)).collect::<Vec<_>>()} key=|s|(s.id.clone(),s.target) children=move |stream|view!{<super::audio::StreamVolume ui=ui stream=stream compact=true/>}/>
        <p class="volume-hint">"Only changes what you hear. 0% mutes."</p>
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
        let channel = target.channel.ok_or("Voice channel unavailable")?;
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
        if own {
            items.push(Item::new(
                "Your audio settings",
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
                    "Change roles…",
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
                        "Disconnect from voice",
                        GuildChange::DisconnectVoice { account_id: member },
                    ),
                    (
                        Permission::KickMembers,
                        "Kick from server",
                        GuildChange::RemoveMember { account_id: member },
                    ),
                    (
                        Permission::BanMembers,
                        "Ban from server",
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
                                    format!("{label}: {}?", m.display_name()),
                                ),
                            )
                            .danger(),
                        );
                    }
                }
                if m.actions.contains(&Permission::ModerateMembers) {
                    items.push(Item::new(
                        "Timeout / moderation…",
                        Action::Page(guild.id, "moderation", Some(member)),
                    ));
                }
            }
        }
        items.push(Item::new("Copy user ID", Action::Copy(member.to_string())));
        return Ok((label, items));
    }
    if let Some(id) = target.channel {
        let channel = home
            .channels
            .into_iter()
            .find(|c| c.id == id)
            .ok_or("Channel is no longer accessible")?;
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
            items.push(Item::new("Mark as read", Action::Read(guild.id, id)));
        }
        if channel.kind == ChannelKind::Voice {
            if ui.voice.get_untracked().channel_id == Some(id) {
                items.push(Item::new("Disconnect", Action::Disconnect));
            } else if permissions.contains(&Permission::JoinVoice) {
                items.push(Item::new(
                    "Join voice",
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
            "Notification settings…",
            Action::Notifications(notification_target, channel.name.clone()),
        ));
        for (duration, label) in thiscord_shared::notifications::Snooze::OPTIONS {
            items.push(Item::new(
                if duration == thiscord_shared::notifications::Snooze::Resume {
                    label.to_owned()
                } else {
                    format!("Mute · {label}")
                },
                Action::Snooze(notification_target, duration),
            ));
        }
        items.push(Item::new(
            if preferences.pinned.contains(&id) {
                "Unpin from top"
            } else {
                "Pin to top"
            },
            Action::Preference(id, true),
        ));
        items.push(Item::new("Copy channel ID", Action::Copy(id.to_string())));
        if home.can_moderate {
            items.push(Item::new(
                "Server moderation…",
                Action::Page(guild.id, "moderation", None),
            ));
        }
        if home.can_manage_roles {
            items.push(Item::new(
                "Channel permissions…",
                Action::ChannelPermissions(guild.id, id),
            ));
        }
        if home.can_manage_channels {
            items.push(
                Item::new(
                    "Delete channel…",
                    Action::Change(
                        guild.id,
                        guild.revision,
                        GuildChange::DeleteChannel { channel_id: id },
                        format!(
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
        "Copy server ID",
        Action::Copy(guild.id.to_string()),
    ));
    items.push(Item::new(
        "Open server",
        Action::Page(guild.id, "servers", None),
    ));
    if home.can_manage_roles {
        items.push(Item::new(
            "Server settings…",
            Action::Page(guild.id, "permissions", None),
        ));
    }
    if home.can_moderate {
        items.push(Item::new(
            "Server moderation…",
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
                "Leave server…",
                Action::Change(
                    guild.id,
                    guild.revision,
                    GuildChange::Leave {},
                    format!(
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
    match action {
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
                Ok(()) => {
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
                Ok(()) => {
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
                let result = super::servers::request(
                    ui,
                    PermissionRequest::Change {
                        guild_id,
                        revision,
                        change,
                    },
                )
                .await;
                ui.status.set(match result {
                    Ok(_) => "Action completed".into(),
                    Err(error) => {
                        format!("{error}. Reopen the menu to refresh permissions before retrying.")
                    }
                });
                super::servers::refresh(ui).await;
                ui.busy.set(false);
            });
        }
    }
}

fn conversation_menu(ui: Ui, event_target: Option<web_sys::EventTarget>, x: f64, y: f64) -> bool {
    let Some(conversation_id) = event_target
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|e| e.closest("[data-context-conversation]").ok().flatten())
        .and_then(|e| e.get_attribute("data-context-conversation"))
        .and_then(|id| id.parse().ok())
    else {
        return false;
    };
    let target = thiscord_shared::notifications::Target::Conversation { conversation_id };
    let title = ui
        .friends
        .snapshot
        .get_untracked()
        .friends
        .into_iter()
        .find(|f| f.conversation_id == conversation_id)
        .map(|f| f.person.display_name)
        .unwrap_or_else(|| "Conversation".into());
    let mut items = vec![Item::new(
        "Notification settings…",
        Action::Notifications(target, title.clone()),
    )];
    for (duration, label) in thiscord_shared::notifications::Snooze::OPTIONS {
        items.push(Item::new(
            if duration == thiscord_shared::notifications::Snooze::Resume {
                label.to_owned()
            } else {
                format!("Mute · {label}")
            },
            Action::Snooze(target, duration),
        ));
    }
    close(ui);
    ui.context_menu.set(Some(Menu {
        title,
        items,
        note: String::new(),
        x,
        y,
        volume: None,
    }));
    true
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let menu_node = NodeRef::<leptos::html::Div>::new();
    let volume = Memo::new(move |_| ui.context_menu.get().and_then(|m| m.volume));
    let position = RwSignal::new((8.0, 8.0));
    let return_focus = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let pending = RwSignal::new(None::<Action>);
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
        if conversation_menu(ui, e.target(), e.client_x().into(), e.client_y().into()) {
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
            if conversation_menu(ui, e.target(), 0.0, 0.0) {
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
                if let Some(menu) = menu_node.get()
                    && let Ok(nodes) =
                        menu.query_selector_all("button[role=menuitem]:not(:disabled)")
                {
                    let buttons = (0..nodes.length())
                        .filter_map(|i| nodes.item(i)?.dyn_into::<web_sys::HtmlElement>().ok())
                        .collect::<Vec<_>>();
                    if !buttons.is_empty() {
                        let current = buttons
                            .iter()
                            .position(|b| document().active_element().as_ref() == Some(b.as_ref()));
                        let index = match e.key().as_str() {
                            "Home" => 0,
                            "End" => buttons.len() - 1,
                            "ArrowUp" => current.map_or(buttons.len() - 1, |i| {
                                (i + buttons.len() - 1) % buttons.len()
                            }),
                            _ => current.map_or(0, |i| (i + 1) % buttons.len()),
                        };
                        let _ = buttons[index].focus();
                    }
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
            Action::Change(..) => {
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
            <div node_ref=menu_node class="app-context-menu" role="menu" aria-label=move ||ui.context_menu.get().map(|m|m.title).unwrap_or_default() style:left=move ||format!("{}px",position.get().0) style:top=move ||format!("{}px",position.get().1)>
                <p class="context-heading">{move ||ui.context_menu.get().map(|m|m.title)}</p>
                {move ||volume.get().map(|(guild,channel,member)|view!{<ParticipantVolume ui=ui guild=guild channel=channel member=member/>})}
                {move ||ui.context_menu.get().map(|m|m.items.into_iter().map(|item|view!{<button role="menuitem" class="context-item" class:context-danger=item.danger disabled=item.disabled on:click=move |_|run(item.action.clone())>{item.label}</button>}).collect_view())}
                <p class="px-2 text-xs text-white/60" role="status">{move ||ui.context_menu.get().map(|m|m.note)}</p>
            </div>
        </Show>
        <dialog node_ref=confirm class="context-dialog" aria-label="Confirm action" on:click=move |e|super::dialog::dismiss_outside(e,confirm,ui.busy.get_untracked()) on:close=move |_|pending.set(None)>
            <h2 class="text-xl font-semibold">{move ||pending.get().and_then(|a|if let Action::Change(_,_,_,title)=a{Some(title)}else{None})}</h2>
            <div class="mt-6 flex justify-end gap-3"><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|if let Some(d)=confirm.get(){d.close();}>"Cancel"</button><button class="rounded bg-red-500/20 px-4 py-2 text-red-200" disabled=move ||ui.busy.get() on:click=move |_|{if let Some(action)=pending.get_untracked(){execute(ui,action);}
                if let Some(d)=confirm.get(){d.close();}}>"Confirm"</button></div>
        </dialog>

    }
}
