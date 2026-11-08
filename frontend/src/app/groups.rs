//! Group conversations: creation and member pickers, the member panel, call
//! state and list rows. Messages reuse the private conversation view in
//! `friends`; the backend authorizes every action (docs/friends.md).
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::{
    AccountId,
    friends::*,
    ids::{CallId, ConversationId},
};
use wasm_bindgen::JsCast;

/// What the shared friend picker is doing.
#[derive(Clone, PartialEq)]
pub(super) enum Picker {
    /// A new group, optionally starting with friends already chosen (such as
    /// the friend of the 1:1 conversation it was opened from).
    Create { preselected: Vec<AccountId> },
    /// More friends for an existing group.
    Add { conversation_id: ConversationId },
}

pub(super) fn me(ui: Ui) -> Option<AccountId> {
    ui.account.with_untracked(|a| a.as_ref().map(|a| a.id))
}
pub(super) fn title(ui: Ui, group: &Group) -> String {
    thiscord_frontend::groups::title(group, me(ui), crate::i18n::language())
}
/// The live call of a group, if any.
pub(super) fn live_call(ui: Ui, id: ConversationId) -> Option<GroupCall> {
    ui.friends.snapshot.with(|s| {
        s.group_calls
            .iter()
            .find(|c| c.conversation_id == id)
            .cloned()
    })
}
/// Whether this device is connected to that call's room.
fn in_room(ui: Ui, call: &GroupCall) -> bool {
    ui.voice
        .with(|v| v.connected && v.channel_id == Some(call.voice_channel()))
}
pub(super) fn open_picker(ui: Ui, picker: Picker) {
    ui.friends.picker.set(Some(picker));
}

/// Starts or joins the group's call from this device.
pub(super) fn call(ui: Ui, id: ConversationId) {
    super::friends::action(
        ui,
        Request::StartCall {
            conversation_id: id,
        },
    );
}
pub(super) fn join(ui: Ui, call: CallId) {
    super::friends::action(ui, Request::AcceptCall { call_id: call });
}

async fn run(ui: Ui, command: Request) -> Result<Response, String> {
    let result = super::friends::request(ui, &command).await;
    if let Err(error) = &result {
        ui.status.set(error.clone());
    }
    result
}
/// Confirms, then leaves; the conversation closes when the socket drops it.
pub(super) fn leave(ui: Ui, id: ConversationId) {
    let name = ui
        .friends
        .group(id)
        .map(|g| title(ui, &g))
        .unwrap_or_default();
    super::dialog::confirm(
        ui,
        super::dialog::Confirmation::new(
            crate::i18n::t("Leave this group?"),
            crate::i18n::t("You will lose access to its messages, files and calls. A member can add you again."),
            crate::i18n::t("Leave group"),
            super::dialog::Action::new(move |_| async move {
                run(ui, Request::LeaveGroup { conversation_id: id }).await.map(|_| {
                    if ui.friends.selected.get_untracked() == Some(id) {
                        ui.friends.selected.set(None);
                        ui.mobile_content.set(false);
                    }
                })
            }),
        )
        .target(name),
    );
}
fn remove(ui: Ui, id: ConversationId, member: AccountId, name: String) {
    super::dialog::confirm(
        ui,
        super::dialog::Confirmation::new(
            crate::i18n::t("Remove from group?"),
            crate::i18n::t("They will lose access to this group's messages, files and calls right away. Their messages stay."),
            crate::i18n::t("Remove"),
            super::dialog::Action::new(move |_| async move {
                let revision = ui.friends.group(id).map(|g| g.revision).unwrap_or_default();
                run(ui, Request::RemoveGroupMember { conversation_id: id, revision, account_id: member }).await.map(|_| ())
            }),
        )
        .target(name),
    );
}
fn transfer(ui: Ui, id: ConversationId, member: AccountId, name: String) {
    super::dialog::confirm(
        ui,
        super::dialog::Confirmation::new(
            crate::i18n::t("Make this member the owner?"),
            crate::i18n::t("The new owner can remove members and transfer ownership. You stay in the group as a member."),
            crate::i18n::t("Transfer ownership"),
            super::dialog::Action::new(move |_| async move {
                let revision = ui.friends.group(id).map(|g| g.revision).unwrap_or_default();
                run(ui, Request::TransferGroup { conversation_id: id, revision, account_id: member }).await.map(|_| ())
            }),
        )
        .target(name),
    );
}

/// Up to two members' avatars, overlapped.
#[component]
pub(super) fn GroupAvatar(
    ui: Ui,
    #[prop(into)] group: Signal<Option<Group>>,
    #[prop(default = false)] large: bool,
) -> impl IntoView {
    let shown = Memo::new(move |_| {
        group.with(|g| {
            g.as_ref()
                .map(|g| {
                    thiscord_frontend::groups::stack(g, me(ui))
                        .into_iter()
                        .map(|m| {
                            (
                                thiscord_frontend::groups::name(m).to_owned(),
                                m.person.avatar_id,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let person = move |index: usize| {
        (
            Signal::derive(move || {
                shown.with(|s| s.get(index).map(|p| p.0.clone()).unwrap_or_default())
            }),
            Signal::derive(move || shown.with(|s| s.get(index).and_then(|p| p.1))),
        )
    };
    let (back_name, back_avatar) = person(0);
    let (front_name, front_avatar) = person(1);
    view! {
        <span class="group-avatar" class:group-avatar-large=large aria-hidden="true">
            <Show when=move || shown.with(|s| s.len() > 1) fallback=move || view! {
                <crate::avatar::Avatar class=if large {"size-16 text-2xl"} else {"size-8 text-xs"} name=back_name avatar_id=back_avatar/>
            }>
                <span class="group-avatar-back"><crate::avatar::Avatar class=if large {"size-11 text-base"} else {"size-6 text-[10px]"} name=back_name avatar_id=back_avatar/></span>
                <span class="group-avatar-front"><crate::avatar::Avatar class=if large {"size-11 text-base"} else {"size-6 text-[10px]"} name=front_name avatar_id=front_avatar/></span>
            </Show>
        </span>
    }
}

/// One group in the direct-message sidebar.
#[component]
pub(super) fn SidebarRow(
    ui: Ui,
    id: ConversationId,
    roving: super::keyboard::Roving<ConversationId>,
) -> impl IntoView {
    let group = Memo::new(move |_| ui.friends.group(id));
    let name = move || group.with(|g| g.as_ref().map(|g| title(ui, g)).unwrap_or_default());
    let count = move || group.with(|g| g.as_ref().map_or(0, |g| g.members.len()));
    let calling = move || live_call(ui, id).is_some();
    view! {
        <button data-rove tabindex=roving.tabindex(id) on:focus=move |_| roving.focus(id)
            aria-current=move || (ui.friends.selected.get() == Some(id)).then_some("page") aria-haspopup="menu"
            data-context-conversation=id.to_string() class="channel-item mb-0.5 gap-3 py-1.5"
            class:channel-selected=move || ui.friends.selected.get() == Some(id) on:click=move |_| super::friends::open(ui, id)>
            <GroupAvatar ui group=group/>
            <span class="min-w-0 flex-1 text-left">
                <span class="block truncate">{name}</span>
                <span class="block truncate text-xs text-muted">{move || if calling() { crate::i18n::t("Call in progress").to_owned() } else { thiscord_frontend::groups::member_count(count(), crate::i18n::language()) }}</span>
            </span>
            <Show when=calling><span class="group-call-dot" role="img" aria-label=move || crate::i18n::t("Call in progress")><Icon kind=Glyph::Phone/></span></Show>
            <Show when=move || super::notifications::muted(ui, thiscord_shared::notifications::Target::Conversation { conversation_id: id })><span role="img" aria-label=move || crate::i18n::t("Notifications muted")><Icon kind=Glyph::BellOff/></span></Show>
            <super::friends::CountBubble count=Signal::derive(move || group.with(|g| g.as_ref().map_or(0, |g| g.unread_count))) label="unread messages"/>
        </button>
    }
}

/// One group in the Friends page's Messages list (the phone conversation list).
#[component]
pub(super) fn ListRow(ui: Ui, id: ConversationId) -> impl IntoView {
    let group = Memo::new(move |_| ui.friends.group(id));
    let name = move || group.with(|g| g.as_ref().map(|g| title(ui, g)).unwrap_or_default());
    let names = move || {
        group.with(|g| {
            g.as_ref()
                .map(|g| {
                    g.members
                        .iter()
                        .map(thiscord_frontend::groups::name)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        })
    };
    let call = Memo::new(move |_| live_call(ui, id));
    view! {
        <div data-rove data-context-conversation=id.to_string() tabindex="0" aria-haspopup="menu" aria-label=name class="friend-row"
            on:click=move |_| super::friends::open(ui, id)
            on:keydown=move |e| if e.key() == "Enter" { super::friends::open(ui, id); }>
            <GroupAvatar ui group=group/>
            <div class="friend-row-name min-w-0 flex-1">
                <p class="truncate font-semibold">{name}</p>
                <p class="truncate text-xs text-white/60">{move || thiscord_frontend::groups::member_count(group.with(|g| g.as_ref().map_or(0, |g| g.members.len())), crate::i18n::language())}" · "{names}</p>
            </div>
            <super::friends::CountBubble count=Signal::derive(move || group.with(|g| g.as_ref().map_or(0, |g| g.unread_count))) label="unread messages"/>
            <Show when=move || call.get().is_some()>
                <span class="group-call-pill"><Icon kind=Glyph::Phone/>{move || crate::i18n::t("Live")}</span>
            </Show>
            <button class="icon-button" aria-label=move || crate::i18n::t("Open group") data-tip=move || crate::i18n::t("Message") on:click=move |e| { e.stop_propagation(); super::friends::open(ui, id); }><Icon kind=Glyph::Message/></button>
        </div>
    }
}

/// The group's call state above its messages: ringing, in progress, or joined.
#[component]
pub(super) fn CallBar(ui: Ui, id: ConversationId) -> impl IntoView {
    let call = Memo::new(move |_| live_call(ui, id));
    let group = Memo::new(move |_| ui.friends.group(id));
    let names = move || {
        let call = call.get();
        let group = group.get();
        match (call, group) {
            // In member (join) order.
            (Some(call), Some(group)) => group
                .members
                .iter()
                .filter(|m| call.participants.contains(&m.person.id))
                .map(|m| thiscord_frontend::groups::name(m).to_owned())
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        }
    };
    view! {
        <Show when=move || call.get().is_some()>
            {move || call.get().map(|c| {
                let joined = in_room(ui, &c);
                let ringing = c.ringing && !joined;
                let call_id = c.id;
                view! {
                    <div class="group-call-bar" class:group-call-ringing=ringing role="status" aria-live="polite">
                        <span class="group-call-icon"><Icon kind=Glyph::Phone/></span>
                        <div class="min-w-0 flex-1">
                            <p class="font-semibold">{if joined { crate::i18n::t("You're in the call") } else if ringing { crate::i18n::t("Incoming group call") } else { crate::i18n::t("Call in progress") }}</p>
                            <p class="truncate text-xs text-muted">{thiscord_frontend::localized_format!(crate::i18n::language(), "{} in call", c.participants.len())}" · "{names}</p>
                        </div>
                        <Show when=move || !joined fallback=move || view! {
                            <button class="btn btn-danger min-h-9 px-3.5" on:click=move |_| super::friends::end_call(ui)><Icon kind=Glyph::Disconnect/>{move || crate::i18n::t("Leave call")}</button>
                        }>
                            <Show when=move || ringing><button class="btn btn-secondary min-h-9 px-3.5" on:click=move |_| super::friends::action(ui, Request::EndCall { call_id })>{move || crate::i18n::t("Decline")}</button></Show>
                            <button class="btn btn-success min-h-9 px-3.5" disabled=move || !super::audio::voice_available(ui) || ui.voice_joining.get() on:click=move |_| join(ui, call_id)><Icon kind=Glyph::Phone/>{move || crate::i18n::t("Join call")}</button>
                        </Show>
                    </div>
                }
            })}
        </Show>
    }
}

/// Inline group title that members can rename in place.
#[component]
pub(super) fn Title(ui: Ui, id: ConversationId) -> impl IntoView {
    let group = Memo::new(move |_| ui.friends.group(id));
    let editing = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let input = NodeRef::<leptos::html::Input>::new();
    let start = move || {
        draft.set(group.with_untracked(|g| g.as_ref().map(|g| g.name.clone()).unwrap_or_default()));
        editing.set(true);
        request_animation_frame(move || {
            if let Some(input) = input.get_untracked() {
                let _ = input.focus();
                input.select();
            }
        });
    };
    let save = move || {
        if busy.get_untracked() {
            return;
        }
        let Some(revision) = group.with_untracked(|g| g.as_ref().map(|g| g.revision)) else {
            return;
        };
        let name = draft.get_untracked().trim().to_owned();
        if group.with_untracked(|g| g.as_ref().is_some_and(|g| g.name == name)) {
            editing.set(false);
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = run(
                ui,
                Request::RenameGroup {
                    conversation_id: id,
                    revision,
                    name,
                },
            )
            .await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            if result.is_ok() {
                editing.set(false);
            }
        });
    };
    view! {
        <Show when=move || editing.get() fallback=move || view! {
            <button type="button" class="group-title" aria-label=move || format!("{} — {}", group.with(|g| g.as_ref().map(|g| title(ui, g)).unwrap_or_default()), crate::i18n::t("Edit group name")) data-tip=move || crate::i18n::t("Edit group name") on:click=move |_| start()>
                <h2 class="pane-title truncate">{move || group.with(|g| g.as_ref().map(|g| title(ui, g)).unwrap_or_default())}</h2>
                <span class="group-title-edit" aria-hidden="true"><Icon kind=Glyph::Edit/></span>
            </button>
        }>
            <form class="min-w-0 flex-1" on:submit=move |e| { e.prevent_default(); save(); }>
                <input node_ref=input class="group-title-input" maxlength="100" aria-label=move || crate::i18n::t("Group name")
                    placeholder=move || crate::i18n::t("Name this group") prop:value=move || draft.get() readonly=move || busy.get()
                    on:input=move |e| draft.set(event_target_value(&e))
                    on:blur=move |_| if editing.get_untracked() && !busy.get_untracked() { save(); }
                    on:keydown=move |e| if e.key() == "Escape" { e.prevent_default(); e.stop_propagation(); editing.set(false); }/>
            </form>
        </Show>
    }
}

/// Group members beside the conversation; a drawer on phones and tablets.
#[component]
pub(super) fn MemberPanel(ui: Ui, id: ConversationId) -> impl IntoView {
    let group = Memo::new(move |_| ui.friends.group(id));
    let owner = Memo::new(move |_| group.with(|g| g.as_ref().and_then(Group::owner)) == me(ui));
    let full = move || {
        group.with(|g| {
            g.as_ref()
                .is_some_and(|g| g.members.len() >= MAX_GROUP_MEMBERS)
        })
    };
    let menu = RwSignal::new(None::<AccountId>);
    let narrow = move || {
        window()
            .inner_width()
            .ok()
            .and_then(|w| w.as_f64())
            .is_some_and(|w| w < 1024.0)
    };
    let close = move || ui.friends.members_open.set(false);
    let call = Memo::new(move |_| live_call(ui, id));
    let roving = super::keyboard::Roving::new(
        move || {
            group.with(|g| {
                g.as_ref()
                    .map(|g| g.members.iter().map(|m| m.person.id).collect())
                    .unwrap_or_default()
            })
        },
        move || None,
    );
    view! {
        <Show when=move || narrow() && ui.friends.members_open.get()>
            <div class="member-drawer-backdrop" aria-hidden="true" on:click=move |_| close()></div>
        </Show>
        <aside class="member-panel" aria-label=move || crate::i18n::t("Group members") class:member-panel-open=move || ui.friends.members_open.get()
            on:keydown=move |e| if e.key() == "Escape" && narrow() { e.prevent_default(); close(); }>
            <header class="member-panel-header">
                <h2 class="pane-title min-w-0 flex-1 truncate">{move || thiscord_frontend::localized_format!(crate::i18n::language(), "Members — {}", group.with(|g| g.as_ref().map_or(0, |g| g.members.len())))}</h2>
                <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Add friends to group") data-tip=move || if full() { crate::i18n::t("This group is full") } else { crate::i18n::t("Add friends") }
                    disabled=full on:click=move |_| open_picker(ui, Picker::Add { conversation_id: id })><Icon kind=Glyph::Plus/></button>
                <button type="button" class="icon-button member-drawer-close" aria-label=move || crate::i18n::t("Close member list") on:click=move |_| close()><Icon kind=Glyph::Close/></button>
            </header>
            <ul class="member-panel-scroll" data-rove-list on:keydown=move |e| super::keyboard::rove(&e)>
                <For each=move || group.with(|g| g.as_ref().map(|g| g.members.clone()).unwrap_or_default()) key=|m| m.person.id children=move |initial| {
                    let account = initial.person.id;
                    let member = Memo::new(move |_| group.with(|g| g.as_ref().and_then(|g| g.member(account).cloned())).unwrap_or_else(|| initial.clone()));
                    let name = Signal::derive(move || thiscord_frontend::groups::name(&member.get()).to_owned());
                    let self_row = me(ui) == Some(account);
                    let calling = move || call.with(|c| c.as_ref().is_some_and(|c| c.participants.contains(&account)));
                    view! {
                        <li class="row-menu">
                            <div data-rove tabindex=roving.tabindex(account) on:focus=move |_| roving.focus(account) class="member-row" class:member-offline=move || !member.get().online
                                data-context-user=account.to_string() data-context-name=name aria-haspopup="menu">
                                <span class="relative shrink-0"><crate::avatar::Avatar class="size-8 text-xs" name=name avatar_id=Signal::derive(move || member.get().person.avatar_id)/><span class="presence-dot" class:online=move || member.get().online></span></span>
                                <span class="min-w-0 flex-1">
                                    <span class="block truncate">{name}{if self_row { format!(" ({})", crate::i18n::t("you")) } else { String::new() }}</span>
                                    <span class="block truncate text-xs text-muted">{move || format!("@{}", member.get().person.username)}</span>
                                </span>
                                <Show when=calling><span class="text-success" role="img" aria-label=move || crate::i18n::t("In the call")><Icon kind=Glyph::Phone/></span></Show>
                                <Show when=move || member.get().owner><span class="text-amber-300" role="img" aria-label=move || crate::i18n::t("Group owner") data-tip=move || crate::i18n::t("Group owner")><Icon kind=Glyph::Crown/></span></Show>
                                <Show when=move || owner.get() && !self_row>
                                    <button type="button" class="icon-button size-8" aria-haspopup="menu" aria-expanded=move || (menu.get() == Some(account)).to_string()
                                        aria-label=move || format!("{} — {}", crate::i18n::t("Member options"), name.get())
                                        on:click=move |e| { e.stop_propagation(); menu.update(|m| *m = if *m == Some(account) { None } else { Some(account) }); }><Icon kind=Glyph::More/></button>
                                </Show>
                            </div>
                            <Show when=move || menu.get() == Some(account)>
                                <div class="row-menu-scrim" aria-hidden="true" on:click=move |_| menu.set(None)></div>
                                <div class="row-menu-panel message-options member-menu" role="menu" aria-label=move || name.get()
                                    on:keydown=move |e| { if e.key() == "Escape" { e.prevent_default(); menu.set(None); } else { super::keyboard::rove_items(&e, "[role=menuitem]"); } }
                                    on:click=move |_| menu.set(None)>
                                    <button type="button" role="menuitem" class="context-item" on:click=move |_| transfer(ui, id, account, name.get_untracked())><Icon kind=Glyph::Crown/>{move || crate::i18n::t("Make group owner")}</button>
                                    <button type="button" role="menuitem" class="context-item context-danger" on:click=move |_| remove(ui, id, account, name.get_untracked())><Icon kind=Glyph::UserMinus/>{move || crate::i18n::t("Remove from group")}</button>
                                </div>
                            </Show>
                        </li>
                    }
                }/>
            </ul>
            <footer class="border-t border-white/10 p-3">
                <button type="button" class="btn btn-secondary w-full justify-center text-red-200" on:click=move |_| leave(ui, id)><Icon kind=Glyph::Logout/>{move || crate::i18n::t("Leave group")}</button>
            </footer>
        </aside>
    }
}

/// The shared multi-select friend picker for creating groups and adding members.
#[component]
pub(super) fn PickerHost(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let query = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let chosen = RwSignal::new(Vec::<AccountId>::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    // Kept across retries of one opened dialog: a lost response returns the same group.
    let client_id = StoredValue::new(None::<thiscord_shared::ClientMessageId>);
    let search = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        let Some(picker) = ui.friends.picker.get() else {
            if let Some(d) = dialog.get_untracked()
                && d.open()
            {
                d.close();
            }
            return;
        };
        query.set(String::new());
        name.set(String::new());
        error.set(String::new());
        busy.set(false);
        client_id.set_value(None);
        chosen.set(match &picker {
            Picker::Create { preselected } => preselected.clone(),
            Picker::Add { .. } => vec![],
        });
        if let Some(d) = dialog.get_untracked() {
            super::dialog::open(&d);
            request_animation_frame(move || {
                if let Some(input) = search.get_untracked() {
                    let _ = input.focus();
                }
            });
        }
    });
    let existing = Memo::new(move |_| match ui.friends.picker.get() {
        Some(Picker::Add { conversation_id }) => ui
            .friends
            .group(conversation_id)
            .map(|g| g.members.iter().map(|m| m.person.id).collect::<Vec<_>>())
            .unwrap_or_default(),
        _ => me(ui).into_iter().collect(),
    });
    let room = Memo::new(move |_| MAX_GROUP_MEMBERS.saturating_sub(existing.with(Vec::len)));
    let friends = Memo::new(move |_| {
        let needle = query.get().trim().to_lowercase();
        ui.friends.snapshot.with(|s| {
            let mut list: Vec<Person> = s
                .friends
                .iter()
                .filter(|f| f.relationship == Relationship::Friend)
                .map(|f| f.person.clone())
                .filter(|p| {
                    needle.is_empty()
                        || p.display_name.to_lowercase().contains(&needle)
                        || p.username.to_lowercase().contains(&needle)
                })
                .collect();
            list.sort_by_key(|p| p.display_name.to_lowercase());
            list
        })
    });
    let person = move |id: AccountId| {
        ui.friends.snapshot.with(|s| {
            s.friends
                .iter()
                .find(|f| f.person.id == id)
                .map(|f| f.person.clone())
        })
    };
    let adding = move || matches!(ui.friends.picker.get(), Some(Picker::Add { .. }));
    let toggle = move |id: AccountId| {
        chosen.update(|c| {
            if let Some(i) = c.iter().position(|x| *x == id) {
                c.remove(i);
            } else if c.len() < room.get_untracked() {
                c.push(id);
            }
        })
    };
    let submit = move || {
        if busy.get_untracked() || chosen.with_untracked(Vec::is_empty) {
            return;
        }
        let Some(picker) = ui.friends.picker.get_untracked() else {
            return;
        };
        let members = chosen.get_untracked();
        let command = match picker {
            Picker::Create { .. } => {
                let id = client_id
                    .get_value()
                    .or_else(|| window().crypto().ok()?.random_uuid().parse().ok());
                let Some(id) = id else {
                    error.set("Could not create a message ID".into());
                    return;
                };
                client_id.set_value(Some(id));
                Request::CreateGroup {
                    client_id: id,
                    members,
                    name: name.get_untracked().trim().to_owned(),
                }
            }
            Picker::Add { conversation_id } => {
                let revision = ui
                    .friends
                    .group(conversation_id)
                    .map(|g| g.revision)
                    .unwrap_or_default();
                Request::AddGroupMembers {
                    conversation_id,
                    revision,
                    members,
                }
            }
        };
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = super::friends::request(ui, &command).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(Response::Group { group }) => {
                    ui.friends.picker.set(None);
                    let id = group.conversation_id;
                    ui.friends.show_group(group);
                    super::friends::open(ui, id);
                }
                Ok(_) => ui.friends.picker.set(None),
                Err(e) => error.set(e),
            }
        });
    };
    view! {
        <dialog node_ref=dialog aria-labelledby="group-picker-title" aria-describedby="group-picker-description" class="app-dialog group-picker max-w-lg"
            on:close=move |_| ui.friends.picker.set(None)
            on:click=move |ev| super::dialog::dismiss_outside(ev, dialog, busy.get_untracked())>
            <header class="mb-2 flex items-start justify-between gap-4">
                <span class="dialog-mark"><Icon kind=Glyph::Users/></span>
                <h2 id="group-picker-title" class="dialog-title mr-auto self-center">{move || if adding() { crate::i18n::t("Add friends") } else { crate::i18n::t("New group") }}</h2>
                <button class="icon-button" aria-label=move || crate::i18n::t("Close") on:click=move |_| ui.friends.picker.set(None)><Icon kind=Glyph::Close/></button>
            </header>
            <p id="group-picker-description" class="dialog-description mb-4">{move || if adding() {
                crate::i18n::t("Choose friends to add. New members can see the whole conversation history.")
            } else {
                crate::i18n::t("Choose up to 7 friends. Everyone can chat, share files and call together.")
            }}</p>
            <form on:submit=move |e| { e.prevent_default(); submit(); }>
                <Show when=move || !adding()>
                    <label class="field mb-3"><span class="field-label">{move || crate::i18n::t("Group name (optional)")}</span>
                        <input class="field-input" maxlength="100" autocomplete="off" placeholder=move || crate::i18n::t("Named after its members")
                            prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/></label>
                </Show>
                <div class="group-picker-search">
                    <Icon kind=Glyph::Search/>
                    <input node_ref=search type="search" class="min-w-0 flex-1 bg-transparent outline-none" autocomplete="off" spellcheck="false"
                        aria-label=move || crate::i18n::t("Search friends") aria-controls="group-picker-list" placeholder=move || crate::i18n::t("Search friends")
                        prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e))
                        on:keydown=move |e| if e.key() == "ArrowDown" {
                            e.prevent_default();
                            if let Some(first) = document().query_selector("#group-picker-list input").ok().flatten().and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) { let _ = first.focus(); }
                        }/>
                </div>
                <Show when=move || !chosen.with(Vec::is_empty)>
                    <ul class="group-picker-chips" aria-label=move || crate::i18n::t("Selected friends")>
                        <For each=move || chosen.get() key=|id| *id children=move |id| {
                            let p = person(id);
                            let label = p.as_ref().map(|p| p.display_name.clone()).unwrap_or_default();
                            let avatar = p.and_then(|p| p.avatar_id);
                            view! {<li class="group-chip">
                                <crate::avatar::Avatar class="size-5 text-[9px]" name=label.clone() avatar_id=avatar/>
                                <span class="max-w-32 truncate">{label.clone()}</span>
                                <button type="button" class="group-chip-remove" aria-label=format!("{} {}", crate::i18n::t("Remove"), label) on:click=move |_| toggle(id)><Icon kind=Glyph::Close/></button>
                            </li>}
                        }/>
                    </ul>
                </Show>
                <ul id="group-picker-list" class="group-picker-list" aria-label=move || crate::i18n::t("Friends")
                    on:keydown=move |e| super::keyboard::rove_items(&e, "input[type=checkbox]:not(:disabled)")>
                    <Show when=move || friends.with(Vec::is_empty)>
                        <li class="px-3 py-6 text-center text-sm text-muted">{move || if query.with(|q| q.trim().is_empty()) { crate::i18n::t("Add friends first to start a group.") } else { crate::i18n::t("No friends match your search.") }}</li>
                    </Show>
                    <For each=move || friends.get() key=|p| p.id children=move |p| {
                        let id = p.id;
                        let member = move || existing.with(|e| e.contains(&id));
                        let checked = move || member() || chosen.with(|c| c.contains(&id));
                        let disabled = move || member() || (!chosen.with(|c| c.contains(&id)) && chosen.with(Vec::len) >= room.get());
                        view! {
                            <li><label class="group-picker-row" class:group-picker-disabled=disabled>
                                <crate::avatar::Avatar class="size-9 text-xs" name=p.display_name.clone() avatar_id=p.avatar_id/>
                                <span class="min-w-0 flex-1"><span class="block truncate font-medium">{p.display_name.clone()}</span>
                                    <span class="block truncate text-xs text-muted">{let username = format!("@{}", p.username); move || if adding() && member() { crate::i18n::t("Already in this group").to_owned() } else { username.clone() }}</span></span>
                                <input type="checkbox" class="group-picker-check" prop:checked=checked disabled=disabled on:change=move |_| toggle(id)
                                    on:keydown=move |e| if e.key() == "Enter" { e.prevent_default(); toggle(id); }/>
                            </label></li>
                        }
                    }/>
                </ul>
                <p role="alert" class="mt-2 min-h-5 text-sm text-red-200">{move || crate::i18n::message(error.get())}</p>
                <footer class="mt-2 flex items-center gap-3">
                    <span class="mr-auto text-sm text-muted" role="status">{move || thiscord_frontend::localized_format!(crate::i18n::language(), "{} of {} selected", chosen.with(Vec::len), room.get())}</span>
                    <button type="button" class="btn btn-secondary" on:click=move |_| ui.friends.picker.set(None)>{move || crate::i18n::t("Cancel")}</button>
                    <button class="btn btn-primary" disabled=move || busy.get() || chosen.with(Vec::is_empty) || !ui.friends.connected.get()>
                        {move || if busy.get() { crate::i18n::t("Saving…") } else if adding() { crate::i18n::t("Add to group") } else { crate::i18n::t("Create group") }}
                    </button>
                </footer>
            </form>
        </dialog>
    }
}

/// Typing line for a group, by member names.
pub(super) fn typing(ui: Ui, id: ConversationId, typing: &[AccountId]) -> Option<String> {
    let group = ui.friends.group(id)?;
    let names: Vec<String> = typing
        .iter()
        .filter_map(|p| {
            group
                .member(*p)
                .map(|m| thiscord_frontend::groups::name(m).to_owned())
        })
        .collect();
    thiscord_frontend::groups::typing(&names, crate::i18n::language())
}
