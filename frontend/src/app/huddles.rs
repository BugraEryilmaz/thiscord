//! Voice huddles (docs/huddles.md): plan sub-groups of a voice channel, then
//! split it so each huddle only hears itself, and bring everyone back with the
//! same button or shortcut. Nobody reconnects; the SFU stops forwarding voices
//! between huddles. Also moving people between voice channels by dragging them
//! in the sidebar. The backend checks every change; these checks only decide
//! which controls to show.
use super::{
    Ui, client,
    icons::{Glyph, Icon},
};
use leptos::{prelude::*, wasm_bindgen::JsCast};
use serde_json::json;
use std::collections::BTreeMap;
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    huddles::*,
    permissions::{GuildChange, PermissionRequest, PermissionResponse},
    voice::Participant,
};

/// A change the server accepted, shown until rosters (polled about once a
/// second) catch up, so dragging someone feels immediate.
#[derive(Clone, PartialEq)]
struct Pending {
    channel: ChannelId,
    state: HuddleState,
    at: f64,
}
const PENDING_MS: f64 = 3000.0;

/// Someone being dragged out of a sidebar voice roster.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Dragged {
    pub guild: GuildId,
    pub channel: ChannelId,
    pub account: AccountId,
}

thread_local! {
    static PENDING: ArcRwSignal<Option<Pending>> = ArcRwSignal::new(None);
    static DRAGGED: ArcRwSignal<Option<Dragged>> = ArcRwSignal::new(None);
}
fn pending() -> ArcRwSignal<Option<Pending>> {
    PENDING.with(Clone::clone)
}
pub(super) fn dragged() -> ArcRwSignal<Option<Dragged>> {
    DRAGGED.with(Clone::clone)
}

/// `Huddle 2`, or `Main room` for 0.
pub(super) fn name(huddle: u8) -> String {
    if huddle == 0 {
        crate::i18n::t("Main room").to_owned()
    } else {
        thiscord_frontend::localized_format!(crate::i18n::language(), "Huddle {}", huddle)
    }
}

/// Rosters with any accepted change that they do not show yet.
pub(super) fn overlay(channel: ChannelId, members: Vec<Participant>) -> Vec<Participant> {
    let Some(p) = pending()
        .get()
        .filter(|p| p.channel == channel && js_sys::Date::now() - p.at < PENDING_MS)
    else {
        return members;
    };
    let assigned: BTreeMap<_, _> = p
        .state
        .assignments
        .iter()
        .map(|a| (a.account_id, a.huddle))
        .collect();
    members
        .into_iter()
        .map(|mut m| {
            m.huddle = assigned.get(&m.account_id).copied().unwrap_or(0);
            m.split = p.state.split;
            m
        })
        .collect()
}

fn state_of(members: &[Participant]) -> HuddleState {
    let mut assignments: Vec<_> = members
        .iter()
        .filter(|m| m.huddle != 0)
        .map(|m| HuddleAssignment {
            account_id: m.account_id,
            huddle: m.huddle,
        })
        .collect();
    assignments.sort_by_key(|a| a.account_id);
    HuddleState {
        split: members.iter().any(|m| m.split),
        assignments,
    }
}

/// The connected guild voice call.
fn call(ui: Ui) -> Option<(GuildId, ChannelId)> {
    let channel = ui.voice.with(|v| v.channel_id.filter(|_| v.connected))?;
    ui.voice_location
        .get()
        .filter(|l| l.channel_id == channel && l.guild_id.as_uuid() != l.channel_id.as_uuid())
        .map(|l| (l.guild_id, l.channel_id))
}

/// The call's participants, with accepted changes not yet in the roster.
fn participants(ui: Ui) -> Vec<Participant> {
    let Some((_, channel)) = call(ui) else {
        return vec![];
    };
    overlay(channel, ui.voice.with(|v| v.participants.clone()))
}

fn own(ui: Ui) -> Option<AccountId> {
    ui.account.with(|a| a.as_ref().map(|a| a.id))
}

async fn send(
    ui: Ui,
    guild_id: GuildId,
    channel_id: ChannelId,
    change: HuddleChange,
) -> Result<HuddleState, String> {
    let token = ui.token.get_untracked();
    let request = HuddleRequest {
        guild_id,
        channel_id,
        change,
    };
    let state: HuddleState = client::api_request(HUDDLES_PATH, &request, token.as_deref()).await?;
    if ui.voice.get_untracked().channel_id == Some(channel_id) {
        pending().set(Some(Pending {
            channel: channel_id,
            state: state.clone(),
            at: js_sys::Date::now(),
        }));
    }
    Ok(state)
}

/// Splits the connected call into its huddles, or brings everyone back. Used
/// by the planner, the desktop global shortcut and the web client's shortcut.
pub(super) fn toggle(ui: Ui) {
    let Some((guild, channel)) = untrack(|| call(ui)) else {
        ui.status
            .set("Join a server voice channel to use huddles.".into());
        return;
    };
    let current = untrack(|| state_of(&participants(ui)));
    if !current.split && current.assignments.is_empty() {
        ui.status
            .set("Plan huddles first: open Huddles in the voice panel.".into());
        return;
    }
    let active = !current.split;
    leptos::task::spawn_local(async move {
        match send(ui, guild, channel, HuddleChange::Split { active }).await {
            Ok(state) => ui.status.set(
                if state.split {
                    "Split into huddles"
                } else {
                    "Everyone is back together"
                }
                .into(),
            ),
            Err(error) => ui.status.set(error),
        }
    });
}

/// Moves someone dragged onto another voice channel (MoveMembers). Uses the
/// existing moderated move: fresh permissions and revision, then the server
/// reconnects them in the destination.
pub(super) fn move_member(ui: Ui, from: Dragged, to: ChannelId) {
    if from.channel == to {
        return;
    }
    let Some(home) = ui
        .server
        .get_untracked()
        .filter(|home| home.guild.id == from.guild)
    else {
        return;
    };
    let destination = home
        .channels
        .iter()
        .find(|c| c.id == to)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    leptos::task::spawn_local(async move {
        let inspected = super::servers::request(
            ui,
            PermissionRequest::InspectModeration {
                guild_id: from.guild,
            },
        )
        .await;
        let (revision, target) = match inspected {
            Ok(PermissionResponse::Moderation { state }) => {
                let target = state
                    .members
                    .iter()
                    .find(|m| m.account_id == from.account)
                    .filter(|m| {
                        m.actions
                            .contains(&thiscord_shared::permissions::Permission::MoveMembers)
                    })
                    .map(|m| m.display_name().to_owned());
                (state.guild.revision, target)
            }
            Ok(_) => return,
            Err(error) => {
                ui.status.set(error);
                return;
            }
        };
        let Some(target) = target else {
            ui.status
                .set("You can’t move this member to another channel.".into());
            return;
        };
        let result = super::servers::request(
            ui,
            PermissionRequest::Change {
                guild_id: from.guild,
                revision,
                change: GuildChange::MoveVoice {
                    account_id: from.account,
                    source_channel_id: from.channel,
                    channel_id: to,
                },
            },
        )
        .await;
        ui.status.set(match result {
            Ok(_) => thiscord_frontend::localized_format!(
                crate::i18n::language(),
                "Moved {} to {}",
                target,
                destination
            ),
            Err(error) => error,
        });
    });
}

/// Who can manage huddles in which voice channels, remembered per server from
/// each loaded server view (the call may be in a server that is not open).
fn huddle_channels(ui: Ui) -> Memo<BTreeMap<GuildId, Vec<ChannelId>>> {
    let known = RwSignal::new(BTreeMap::<GuildId, Vec<ChannelId>>::new());
    Effect::new(move |_| {
        if let Some(home) = ui.server.get() {
            known.update(|k| {
                k.insert(home.guild.id, home.huddle_channels.clone());
            });
        }
    });
    Memo::new(move |_| known.get())
}

/// One person in the planner: drag between lanes, or press 0-4.
#[component]
fn Person(
    ui: Ui,
    member: Participant,
    picked: RwSignal<Option<AccountId>>,
    assign: Callback<(AccountId, u8)>,
) -> impl IntoView {
    let id = member.account_id;
    let label = member.display_name().to_owned();
    let you = own(ui) == Some(id);
    let shown = label.clone();
    let tip = label.clone();
    view! {
        <button type="button" class="huddle-person" draggable="true" aria-pressed=move || (picked.get() == Some(id)).to_string()
            class:picked=move || picked.get() == Some(id)
            aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "{}, {}. Press 1 to 4 to choose a huddle, 0 for the main room.", tip, name(member.huddle))
            on:dragstart=move |e: web_sys::DragEvent| {
                if let Some(data) = e.data_transfer() {
                    let _ = data.set_data("text/plain", &id.to_string());
                    data.set_effect_allowed("move");
                }
                picked.set(Some(id));
            }
            on:dragend=move |_| picked.set(None)
            on:click=move |_| picked.update(|p| *p = if *p == Some(id) { None } else { Some(id) })
            on:keydown=move |e: web_sys::KeyboardEvent| {
                let key = e.key();
                if let Some(huddle) = key.parse::<u8>().ok().filter(|h| *h <= MAX_HUDDLES) {
                    e.prevent_default();
                    picked.set(None);
                    assign.run((id, huddle));
                } else if key == "Escape" && picked.get_untracked().is_some() {
                    e.prevent_default();
                    e.stop_propagation();
                    picked.set(None);
                }
            }>
            <crate::avatar::Avatar class="size-6 text-xs" name=Signal::derive({let n = label.clone(); move || n.clone()}) avatar_id=Signal::derive(move || member.avatar_id)/>
            <span class="min-w-0 truncate">{shown}</span>
            {you.then(|| view! {<span class="huddle-you">{move || crate::i18n::t("you")}</span>})}
        </button>
    }
}

/// One huddle (or the main room): a drop target listing its people.
#[component]
fn Lane(
    ui: Ui,
    huddle: u8,
    members: Memo<Vec<Participant>>,
    picked: RwSignal<Option<AccountId>>,
    assign: Callback<(AccountId, u8)>,
) -> impl IntoView {
    let over = RwSignal::new(false);
    let people = Memo::new(move |_| {
        members
            .get()
            .into_iter()
            .filter(|m| m.huddle == huddle)
            .collect::<Vec<_>>()
    });
    let drop_here = move || {
        if let Some(id) = picked.get_untracked() {
            picked.set(None);
            assign.run((id, huddle));
        }
    };
    view! {
        <section class=format!("huddle-lane huddle-{huddle}") class:over=move || over.get()
            aria-label=move || name(huddle)
            on:dragover=move |e: web_sys::DragEvent| { if picked.get_untracked().is_some() { e.prevent_default(); over.set(true); } }
            on:dragleave=move |_| over.set(false)
            on:drop=move |e: web_sys::DragEvent| { e.prevent_default(); over.set(false); drop_here(); }>
            <header class="huddle-lane-header">
                <span class="huddle-dot" aria-hidden="true"></span>
                <span class="flex-1 truncate">{move || name(huddle)}</span>
                <Show when=move || picked.get().is_some_and(|id| people.with(|p| !p.iter().any(|m| m.account_id == id)))
                    fallback=move || view! {<span class="huddle-count">{move || people.with(Vec::len)}</span>}>
                    <button type="button" class="huddle-move-here" on:click=move |_| drop_here()>{move || crate::i18n::t("Move here")}</button>
                </Show>
            </header>
            <div class="huddle-people">
                <For each=move || people.get() key=|m| (m.account_id, m.display_name().to_owned(), m.avatar_id) children=move |m| view! {<Person ui member=m picked assign/>}/>
                <Show when=move || people.with(Vec::is_empty)>
                    <p class="huddle-empty">{move || crate::i18n::t("Drag people here")}</p>
                </Show>
            </div>
        </section>
    }
}

/// Voice dock control: opens the planner for people who may manage huddles.
#[component]
pub(super) fn HuddleControls(ui: Ui) -> impl IntoView {
    let open = RwSignal::new(false);
    let picked = RwSignal::new(None::<AccountId>);
    let busy = RwSignal::new(false);
    let notice = RwSignal::new(String::new());
    let shortcut = RwSignal::new(String::new());
    let position = RwSignal::new((8.0, 8.0));
    let group = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let channels = huddle_channels(ui);
    let allowed = move || {
        call(ui).is_some_and(|(guild, channel)| {
            channels.with(|c| c.get(&guild).is_some_and(|c| c.contains(&channel)))
        })
    };
    let members = Memo::new(move |_| {
        let mut members = participants(ui);
        members.sort_by_key(|m| m.display_name().to_lowercase());
        members
    });
    let split = Memo::new(move |_| members.with(|m| m.iter().any(|m| m.split)));
    let planned = Memo::new(move |_| members.with(|m| m.iter().any(|m| m.huddle != 0)));
    let run = move |change: HuddleChange| {
        let Some((guild, channel)) = untrack(|| call(ui)) else {
            return;
        };
        busy.set(true);
        notice.set(String::new());
        leptos::task::spawn_local(async move {
            let result = send(ui, guild, channel, change).await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            if let Err(error) = result {
                notice.set(crate::i18n::message(error));
            }
        });
    };
    let assign = Callback::new(move |(account_id, huddle): (AccountId, u8)| {
        run(HuddleChange::Assign { account_id, huddle });
    });
    // Leaving the call or losing the permission closes the planner.
    Effect::new(move |_| {
        if !allowed() {
            open.set(false);
        }
    });
    let close = move || {
        if let Some(button) = trigger.get_untracked() {
            let _ = button.focus();
        }
        open.set(false);
    };
    Effect::new(move |_| {
        if !open.get() {
            picked.set(None);
            return;
        }
        notice.set(String::new());
        if let Some(button) = trigger.get_untracked() {
            let dock = button
                .closest(".user-dock")
                .ok()
                .flatten()
                .map(|dock| dock.get_bounding_client_rect())
                .unwrap_or_else(|| button.get_bounding_client_rect());
            let size = |value: Result<leptos::wasm_bindgen::JsValue, _>, fallback| {
                value.ok().and_then(|v| v.as_f64()).unwrap_or(fallback)
            };
            let (width, height) = (
                size(window().inner_width(), 800.0),
                size(window().inner_height(), 600.0),
            );
            // The planner is at most 520 px wide (style.css); keep it on screen.
            position.set((
                (dock.right() + 8.0).min(width - 528.0).max(8.0),
                (height - dock.bottom()).max(8.0),
            ));
        }
        leptos::task::spawn_local(async move {
            if let Ok(status) = ui
                .audio_control
                .invoke::<thiscord_shared::audio::AudioShortcutStatus>("audio_shortcuts", json!({}))
                .await
                && !shortcut.is_disposed()
            {
                shortcut.set(status.shortcuts.huddles);
            }
        });
        let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
            let inside = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|target| {
                    group
                        .get_untracked()
                        .is_some_and(|group| group.contains(Some(&target)))
                });
            if !inside {
                open.set(false);
            }
        });
        let resize = window_event_listener(leptos::ev::resize, move |_| open.set(false));
        on_cleanup(move || {
            outside.remove();
            resize.remove();
        });
    });
    view! {<Show when=allowed>
        <div node_ref=group class="contents" on:keydown=move |e| {
            if e.key() == "Escape" && open.get_untracked() { e.prevent_default(); e.stop_propagation(); close(); }
        }>
            <button node_ref=trigger type="button" class="icon-button voice-action" class:huddles-split=move || split.get()
                aria-label=move || crate::i18n::t("Huddles") data-tip=move || crate::i18n::t(if split.get() { "Huddles · split" } else { "Huddles" })
                aria-haspopup="dialog" aria-expanded=move || open.get().to_string()
                on:click=move |_| open.update(|value| *value = !*value)><Icon kind=Glyph::Grid/></button>
            <Show when=move || open.get()>
                <div class="huddle-popover" role="dialog" aria-labelledby="huddle-title"
                    style:left=move || format!("{}px", position.get().0) style:bottom=move || format!("{}px", position.get().1)>
                    <div class="huddle-head">
                        <h2 id="huddle-title" class="soundboard-title !mb-0"><Icon kind=Glyph::Grid/><span>{move || crate::i18n::t("Huddles")}</span></h2>
                        <span class="huddle-state" class:split=move || split.get() role="status">{move || crate::i18n::t(if split.get() { "Split" } else { "Together" })}</span>
                    </div>
                    <p class="huddle-help">{move || crate::i18n::t("Drag people into huddles, or select someone and press 1–4. When split, each huddle hears only itself; screens and cameras stay visible.")}</p>
                    <Lane ui huddle=0 members picked assign/>
                    <div class="huddle-grid">
                        {(1..=MAX_HUDDLES).map(|huddle| view! {<Lane ui huddle members picked assign/>}).collect_view()}
                    </div>
                    <div class="huddle-actions">
                        <button type="button" class="huddle-toggle" class:split=move || split.get()
                            disabled=move || busy.get() || (!split.get() && !planned.get())
                            data-tip=move || (!split.get() && !planned.get()).then(|| crate::i18n::t("Put at least one person in a huddle first"))
                            on:click=move |_| run(HuddleChange::Split { active: !split.get_untracked() })>
                            {move || crate::i18n::t(if split.get() { "Bring everyone back" } else { "Split into huddles" })}
                        </button>
                        <button type="button" class="settings-secondary" disabled=move || busy.get() || (!split.get() && !planned.get())
                            on:click=move |_| run(HuddleChange::Clear {})>{move || crate::i18n::t("Clear huddles")}</button>
                    </div>
                    <p class="huddle-shortcut">
                        {move || {
                            let keys = thiscord_frontend::web_shortcuts::labels(&shortcut.get());
                            if keys.is_empty() {
                                view! {<span>{move || crate::i18n::t("No shortcut yet.")}</span>}.into_any()
                            } else {
                                view! {<span class="flex flex-wrap items-center gap-1">{move || crate::i18n::t("Shortcut:")}{keys.into_iter().map(|k| view! {<kbd>{k}</kbd>}).collect_view()}</span>}.into_any()
                            }
                        }}
                        <button type="button" class="huddle-link" on:click=move |_| { open.set(false); ui.page.set("shortcuts"); }>
                            {move || crate::i18n::t(if shortcut.with(String::is_empty) { "Set a shortcut" } else { "Change" })}
                        </button>
                    </p>
                    <Show when=move || !notice.get().is_empty()><p class="soundboard-notice" role="alert">{move || notice.get()}</p></Show>
                </div>
            </Show>
        </div>
    </Show>}
}

/// Tells everyone in a split call which huddle they are in and who hears them.
#[component]
pub(super) fn HuddleStatus(ui: Ui) -> impl IntoView {
    let line = Memo::new(move |_| {
        let members = participants(ui);
        let me = own(ui)?;
        let mine = members.iter().find(|m| m.account_id == me && m.split)?;
        let others: Vec<_> = members
            .iter()
            .filter(|m| m.account_id != me && m.hears(mine))
            .map(|m| m.display_name().to_owned())
            .collect();
        Some((mine.huddle, others))
    });
    view! {<Show when=move || line.with(Option::is_some)>
        <p class=move || format!("huddle-status huddle-{}", line.get().map_or(0, |l| l.0)) role="status">
            <span class="huddle-dot" aria-hidden="true"></span>
            <span class="min-w-0 truncate">{move || line.get().map(|(huddle, others)| if others.is_empty() {
                thiscord_frontend::localized_format!(crate::i18n::language(), "{} · only you", name(huddle))
            } else {
                thiscord_frontend::localized_format!(crate::i18n::language(), "{} · with {}", name(huddle), others.join(", "))
            })}</span>
        </p>
    </Show>}
}

/// Toggles huddles when the desktop global shortcut fires. Mounted once with
/// the voice dock; the web client handles its own keys in `audio::browser_key`.
#[component]
pub(super) fn HuddleShortcut(ui: Ui) -> impl IntoView {
    Effect::new(move |_| forget(ui.voice.with(|v| v.channel_id)));
    if client::desktop() {
        let listener = StoredValue::new_local(None::<client::EventListener>);
        leptos::task::spawn_local(async move {
            let result =
                client::listen(thiscord_frontend::web_shortcuts::HUDDLES_EVENT, move |()| {
                    toggle(ui)
                })
                .await;
            if let Ok(handle) = result
                && !listener.is_disposed()
            {
                listener.set_value(Some(handle));
            }
        });
    }
}

/// Keep `PENDING` from outliving the call it describes.
pub(super) fn forget(channel: Option<ChannelId>) {
    let pending = pending();
    if pending
        .get_untracked()
        .is_some_and(|p| Some(p.channel) != channel)
    {
        pending.set(None);
    }
}
