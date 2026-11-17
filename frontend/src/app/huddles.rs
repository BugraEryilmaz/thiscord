//! Voice huddles (docs/huddles.md): plan sub-groups of a voice channel, then
//! split it so each huddle only hears itself, and bring everyone back with the
//! same button or shortcut. Nobody reconnects; the SFU stops forwarding voices
//! between huddles. Also moving people between voice channels by dragging them
//! in the sidebar. The backend checks every change; these checks only decide
//! which controls to show. Pure logic lives in `thiscord_frontend::huddles`.
use super::{
    Ui, client,
    icons::{Glyph, Icon},
};
use leptos::{prelude::*, wasm_bindgen::JsCast};
use serde_json::json;
use std::collections::BTreeMap;
use thiscord_frontend::huddles::{self as logic, state_of};
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    huddles::*,
    permissions::{GuildChange, Permission, PermissionRequest, PermissionResponse},
    voice::Participant,
};

/// A change the server accepted, shown until rosters (polled about once a
/// second) catch up, so dragging someone feels immediate.
#[derive(Clone, PartialEq)]
struct Pending {
    channel: ChannelId,
    change: logic::Pending,
    at: f64,
}
/// A safety net only: the overlay ends as soon as rosters catch up or differ.
const PENDING_MS: f64 = 15_000.0;

/// Someone being dragged out of a sidebar voice roster.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Dragged {
    pub guild: GuildId,
    pub channel: ChannelId,
    pub account: AccountId,
}

/// Who the viewer may move right now (rank and MoveMembers), from the
/// moderation view, with the guild revision a move must submit.
#[derive(Clone, PartialEq)]
struct Movable {
    guild: GuildId,
    revision: i64,
    /// Each movable person and the voice channels a move could use (empty
    /// from older servers: unknown).
    accounts: BTreeMap<AccountId, Vec<ChannelId>>,
    at: f64,
}
const MOVABLE_MS: f64 = 10_000.0;

thread_local! {
    static PENDING: ArcRwSignal<Option<Pending>> = ArcRwSignal::new(None);
    static DRAGGED: ArcRwSignal<Option<Dragged>> = ArcRwSignal::new(None);
    static MOVABLE: ArcRwSignal<Option<Movable>> = ArcRwSignal::new(None);
    static MOVABLE_LOADING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn pending() -> ArcRwSignal<Option<Pending>> {
    PENDING.with(Clone::clone)
}
pub(super) fn dragged() -> ArcRwSignal<Option<Dragged>> {
    DRAGGED.with(Clone::clone)
}
fn movable_state() -> ArcRwSignal<Option<Movable>> {
    MOVABLE.with(Clone::clone)
}

/// `Huddle 2`, or `Main room` for 0.
pub(super) fn name(huddle: u8) -> String {
    if huddle == 0 {
        crate::i18n::t("Main room").to_owned()
    } else {
        thiscord_frontend::localized_format!(crate::i18n::language(), "Huddle {}", huddle)
    }
}

/// Rosters with an accepted change that they do not show yet.
pub(super) fn overlay(channel: ChannelId, members: Vec<Participant>) -> Vec<Participant> {
    match pending()
        .get()
        .filter(|p| p.channel == channel && js_sys::Date::now() - p.at < PENDING_MS)
    {
        Some(p) => p.change.overlay(members),
        None => members,
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

/// A call participant's or server member's display name.
fn display(ui: Ui, guild: Option<GuildId>, account: AccountId) -> String {
    ui.voice
        .with_untracked(|v| {
            v.participants
                .iter()
                .find(|m| m.account_id == account)
                .map(|m| m.display_name().to_owned())
        })
        .or_else(|| guild.and_then(|g| ui.members.name(g, account)))
        .unwrap_or_default()
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
        let earlier = pending()
            .get_untracked()
            .filter(|p| p.channel == channel_id && js_sys::Date::now() - p.at < PENDING_MS);
        let change = ui.voice.with_untracked(|v| {
            logic::chain(
                earlier.as_ref().map(|p| &p.change),
                &v.participants,
                state.clone(),
            )
        });
        pending().set(Some(Pending {
            channel: channel_id,
            change,
            at: js_sys::Date::now(),
        }));
    }
    Ok(state)
}

/// Splits the connected call into its huddles, or brings everyone back. Used
/// by the planner, the desktop global shortcut and the web client's shortcut.
/// Success is told by `HuddleStatus` (cue, dock line, announcement); every
/// failure plays the error cue so a player in a full-screen game notices it.
pub(super) fn toggle(ui: Ui) {
    let Some((guild, channel)) = untrack(|| call(ui)) else {
        cue(ui, Cue::Error);
        ui.status
            .set("Join a server voice channel to use huddles.".into());
        return;
    };
    let Some(active) = logic::toggle(&untrack(|| state_of(&participants(ui)))) else {
        cue(ui, Cue::Error);
        ui.status
            .set("Plan huddles first: open Huddles in the voice panel.".into());
        return;
    };
    leptos::task::spawn_local(async move {
        if let Err(error) = send(ui, guild, channel, HuddleChange::Split { active }).await {
            cue(ui, Cue::Error);
            ui.status.set(error);
        }
    });
}

/// Short tones, distinct from the join/leave sounds: falling for a split,
/// rising for a regroup, a double blip when moved, a low buzz for a failure.
#[derive(Clone, Copy)]
enum Cue {
    Split,
    Regroup,
    Moved,
    Error,
}

thread_local! {
    static CUES: std::cell::RefCell<Option<web_sys::AudioContext>> = const { std::cell::RefCell::new(None) };
}

fn cue(ui: Ui, cue: Cue) {
    let deafened = ui.audio.with_untracked(|a| a.deafened)
        || ui.voice.with_untracked(|v| {
            v.participants
                .iter()
                .any(|m| Some(m.slot) == v.own_slot && m.server_deafened)
        });
    if deafened {
        return;
    }
    let (notes, wave): (&[(f32, f64)], _) = match cue {
        Cue::Split => (
            &[(784.0, 0.0), (523.0, 0.11)],
            web_sys::OscillatorType::Sine,
        ),
        Cue::Regroup => (
            &[(523.0, 0.0), (784.0, 0.11)],
            web_sys::OscillatorType::Sine,
        ),
        Cue::Moved => (
            &[(659.0, 0.0), (659.0, 0.13)],
            web_sys::OscillatorType::Sine,
        ),
        Cue::Error => (
            &[(196.0, 0.0), (185.0, 0.14)],
            web_sys::OscillatorType::Triangle,
        ),
    };
    CUES.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = web_sys::AudioContext::new().ok();
        }
        let Some(context) = slot.as_ref() else {
            return;
        };
        let _ = context.resume();
        let start = context.current_time();
        for &(frequency, offset) in notes {
            let (Ok(oscillator), Ok(gain)) = (context.create_oscillator(), context.create_gain())
            else {
                return;
            };
            oscillator.set_type(wave);
            oscillator.frequency().set_value(frequency);
            let at = start + offset;
            // Same peak as the notification palette; a short attack and decay.
            let _ = gain.gain().set_value_at_time(0.0, at);
            let _ = gain.gain().linear_ramp_to_value_at_time(0.12, at + 0.012);
            let _ = gain
                .gain()
                .exponential_ramp_to_value_at_time(0.0005, at + 0.1);
            if oscillator.connect_with_audio_node(&gain).is_err()
                || gain
                    .connect_with_audio_node(&context.destination())
                    .is_err()
            {
                return;
            }
            let _ = oscillator.start_with_when(at);
            let _ = oscillator.stop_with_when(at + 0.11);
        }
    });
}

/// Loads who the viewer may move in `guild`, at most every ten seconds. Called
/// when the pointer reaches a voice roster, so rows are draggable only for
/// people the server would let you move.
pub(super) fn refresh_movable(ui: Ui, guild: GuildId) {
    let fresh = movable_state()
        .get_untracked()
        .is_some_and(|m| m.guild == guild && js_sys::Date::now() - m.at < MOVABLE_MS);
    if fresh || MOVABLE_LOADING.with(|l| l.replace(true)) {
        return;
    }
    leptos::task::spawn_local(async move {
        let result =
            super::servers::request(ui, PermissionRequest::InspectModeration { guild_id: guild })
                .await;
        MOVABLE_LOADING.with(|l| l.set(false));
        if let Ok(PermissionResponse::Moderation { state }) = result {
            movable_state().set(Some(Movable {
                guild,
                revision: state.guild.revision,
                accounts: state
                    .members
                    .iter()
                    .filter(|m| m.actions.contains(&Permission::MoveMembers))
                    .map(|m| (m.account_id, m.move_channels.clone()))
                    .collect(),
                at: js_sys::Date::now(),
            }));
        }
    });
}

/// Whether the viewer may move `account` (tracked; false until loaded).
pub(super) fn movable(guild: GuildId, account: AccountId) -> bool {
    movable_state()
        .get()
        .is_some_and(|m| m.guild == guild && m.accounts.contains_key(&account))
}

/// Whether a dragged person may be dropped on `channel`: another voice channel
/// of the same server with room for them.
pub(super) fn accepts(
    ui: Ui,
    dragged: Option<Dragged>,
    guild: GuildId,
    channel: ChannelId,
) -> bool {
    let Some(d) = dragged.filter(|d| d.guild == guild) else {
        return false;
    };
    let occupants = ui.voice_roster.with(|r| {
        r.iter()
            .find(|c| c.channel_id == channel)
            .map_or(0, |c| c.members.len())
    });
    let allowed = movable_state()
        .with(|m| m.as_ref().and_then(|m| m.accounts.get(&d.account).cloned()))
        .unwrap_or_default();
    logic::can_drop(d.channel, channel, occupants, &allowed)
}

/// Someone being moved, shown in the destination roster until they arrive.
#[derive(Clone, PartialEq)]
pub(super) struct Moving {
    pub to: ChannelId,
    pub name: String,
    pub avatar: Option<thiscord_shared::AvatarId>,
}
thread_local! {
    static MOVING: ArcRwSignal<BTreeMap<AccountId, Moving>> = ArcRwSignal::new(BTreeMap::new());
    static PLANNING: ArcRwSignal<bool> = ArcRwSignal::new(false);
}
/// Whether this window's Huddles planner is open: the sidebar shows planned
/// huddle numbers only then.
pub(super) fn planning() -> bool {
    PLANNING.with(|p| p.get())
}
pub(super) fn moving() -> ArcRwSignal<BTreeMap<AccountId, Moving>> {
    MOVING.with(Clone::clone)
}

/// Moves someone dragged onto another voice channel (MoveMembers). Uses the
/// existing moderated move with fresh server checks; the server reconnects them
/// in the destination.
pub(super) fn move_member(ui: Ui, from: Dragged, to: ChannelId) {
    if from.channel == to {
        return;
    }
    let destination = ui
        .server
        .with_untracked(|home| {
            home.as_ref()
                .filter(|home| home.guild.id == from.guild)
                .and_then(|home| home.channels.iter().find(|c| c.id == to))
                .map(|c| c.name.clone())
        })
        .unwrap_or_default();
    let target = display(ui, Some(from.guild), from.account);
    ui.status.set(thiscord_frontend::localized_format!(
        crate::i18n::language(),
        "Moving {} to {}…",
        target,
        destination
    ));
    let cached = movable_state()
        .get_untracked()
        .filter(|m| m.guild == from.guild && js_sys::Date::now() - m.at < MOVABLE_MS);
    leptos::task::spawn_local(async move {
        let revision = match cached {
            Some(m) => m.accounts.contains_key(&from.account).then_some(m.revision),
            None => match super::servers::request(
                ui,
                PermissionRequest::InspectModeration {
                    guild_id: from.guild,
                },
            )
            .await
            {
                Ok(PermissionResponse::Moderation { state }) => state
                    .members
                    .iter()
                    .any(|m| {
                        m.account_id == from.account && m.actions.contains(&Permission::MoveMembers)
                    })
                    .then_some(state.guild.revision),
                Ok(_) => None,
                Err(error) => {
                    ui.status.set(error);
                    return;
                }
            },
        };
        let Some(revision) = revision else {
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
        // The move changed the guild revision; reload before the next drag.
        movable_state().set(None);
        if let Err(error) = result {
            ui.status.set(error);
            return;
        }
        // Keep "Moving…" until the destination roster shows them (they
        // reconnect there), at most ten seconds.
        let avatar = ui.voice_roster.with_untracked(|r| {
            r.iter()
                .flat_map(|c| &c.members)
                .find(|m| m.account_id == from.account)
                .and_then(|m| m.avatar_id)
        });
        moving().update(|m| {
            m.insert(
                from.account,
                Moving {
                    to,
                    name: target.clone(),
                    avatar,
                },
            );
        });
        let mut arrived = false;
        for _ in 0..40 {
            gloo_timers::future::TimeoutFuture::new(250).await;
            arrived = ui.voice_roster.with_untracked(|r| {
                r.iter().any(|c| {
                    c.channel_id == to && c.members.iter().any(|m| m.account_id == from.account)
                })
            });
            if arrived {
                break;
            }
        }
        moving().update(|m| {
            if m.get(&from.account).is_some_and(|m| m.to == to) {
                m.remove(&from.account);
            }
        });
        let lang = crate::i18n::language();
        ui.status.set(if arrived {
            thiscord_frontend::localized_format!(lang, "Moved {} to {}", target, destination)
        } else {
            thiscord_frontend::localized_format!(
                lang,
                "{} was moved but has not reached {} yet",
                target,
                destination
            )
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
    refocus: RwSignal<Option<AccountId>>,
    assign: Callback<(AccountId, u8)>,
) -> impl IntoView {
    let id = member.account_id;
    let label = member.display_name().to_owned();
    let you = own(ui) == Some(id);
    let shown = label.clone();
    let tip = label.clone();
    let button = NodeRef::<leptos::html::Button>::new();
    // Moving someone re-renders them in another lane; keep keyboard focus on them.
    Effect::new(move |_| {
        if refocus.get() == Some(id)
            && let Some(button) = button.get()
        {
            let _ = button.focus();
            refocus.set(None);
        }
    });
    view! {
        <button node_ref=button type="button" class="huddle-person" draggable="true" data-account=id.to_string() aria-pressed=move || (picked.get() == Some(id)).to_string()
            class:picked=move || picked.get() == Some(id)
            aria-label=move || format!("{tip}, {}", name(member.huddle))
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
                } else if let Some(step) = match key.as_str() { "ArrowRight" | "ArrowDown" => Some(1), "ArrowLeft" | "ArrowUp" => Some(-1), _ => None } {
                    e.prevent_default();
                    step_focus(&e, step);
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

/// Moves focus to the previous/next person in the planner, across lanes.
fn step_focus(e: &web_sys::KeyboardEvent, step: i32) {
    let Some(current) = e
        .current_target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let Some(dialog) = current.closest(".huddle-popover").ok().flatten() else {
        return;
    };
    let Ok(people) = dialog.query_selector_all(".huddle-person") else {
        return;
    };
    let people: Vec<web_sys::HtmlElement> = (0..people.length())
        .filter_map(|i| people.item(i)?.dyn_into().ok())
        .collect();
    let Some(index) = people
        .iter()
        .position(|p| p.is_same_node(Some(current.unchecked_ref())))
    else {
        return;
    };
    let next = (index as i32 + step).rem_euclid(people.len() as i32) as usize;
    let _ = people[next].focus();
}

/// One huddle (or the main room): a drop target listing its people.
#[component]
fn Lane(
    ui: Ui,
    huddle: u8,
    members: Memo<Vec<Participant>>,
    picked: RwSignal<Option<AccountId>>,
    refocus: RwSignal<Option<AccountId>>,
    assign: Callback<(AccountId, u8)>,
) -> impl IntoView {
    let over = RwSignal::new(false);
    let picked_name = move || {
        let id = picked.get()?;
        members.with(|m| {
            m.iter()
                .find(|m| m.account_id == id)
                .map(|m| m.display_name().to_owned())
        })
    };
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
                    <button type="button" class="huddle-move-here" on:click=move |_| drop_here()
                        aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "Move {} to {}", picked_name().unwrap_or_default(), name(huddle))>{move || crate::i18n::t("Move here")}</button>
                </Show>
            </header>
            <div class="huddle-people">
                <For each=move || people.get() key=|m| (m.account_id, m.display_name().to_owned(), m.avatar_id) children=move |m| view! {<Person ui member=m picked refocus assign/>}/>
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
    // Recording the shortcut right here, without leaving the planner.
    let saved_shortcuts = RwSignal::new(None::<thiscord_shared::audio::AudioShortcuts>);
    let recording = RwSignal::new(false);
    let shortcut_busy = RwSignal::new(false);
    let shortcut_dirty = RwSignal::new(false);
    let shortcut_message = RwSignal::new(String::new());
    let save_shortcut = move || {
        let Some(mut settings) = saved_shortcuts.get_untracked() else {
            return;
        };
        settings.huddles = shortcut.get_untracked().trim().to_owned();
        let voice = [
            &settings.push_to_talk,
            &settings.mute,
            &settings.deafen,
            &settings.overlay_visibility,
            &settings.overlay_corner,
            &settings.overlay_unlock,
            &settings.huddles,
        ]
        .map(String::as_str);
        if let Err(error) = super::keyboard::bindings_untracked().validate(&voice) {
            shortcut_message.set(error.into());
            return;
        }
        shortcut_busy.set(true);
        leptos::task::spawn_local(async move {
            let result = ui
                .audio_control
                .invoke::<()>("audio_shortcuts_save", json!({ "settings": settings }))
                .await;
            if shortcut_busy.is_disposed() {
                return;
            }
            shortcut_busy.set(false);
            match result {
                Ok(()) => {
                    saved_shortcuts.set(Some(settings));
                    shortcut_dirty.set(false);
                    recording.set(false);
                    shortcut_message.set(String::new());
                }
                Err(error) => shortcut_message.set(error),
            }
        });
    };
    let refocus = RwSignal::new(None::<AccountId>);
    let announce = RwSignal::new(String::new());
    let position = RwSignal::new((8.0, 8.0));
    let group = NodeRef::<leptos::html::Div>::new();
    let dialog = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let channels = huddle_channels(ui);
    let allowed = move || {
        call(ui).is_some_and(|(guild, channel)| {
            channels.with(|c| c.get(&guild).is_some_and(|c| c.contains(&channel)))
        })
    };
    // Roster order, the same as the sidebar.
    let members = Memo::new(move |_| participants(ui));
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
            match result {
                Ok(state) => {
                    announce.set(match change {
                        HuddleChange::Assign { account_id, huddle } => {
                            refocus.set(Some(account_id));
                            let who = members.with_untracked(|m| {
                                m.iter()
                                    .find(|m| m.account_id == account_id)
                                    .map(|m| m.display_name().to_owned())
                                    .unwrap_or_default()
                            });
                            thiscord_frontend::localized_format!(
                                crate::i18n::language(),
                                "{} is now in {}",
                                who,
                                name(huddle)
                            )
                        }
                        // HuddleStatus cues and announces splits for everyone.
                        HuddleChange::Split { .. } => {
                            let _ = state;
                            String::new()
                        }
                        HuddleChange::Clear {} => crate::i18n::t("Huddles cleared").to_owned(),
                    });
                }
                Err(error) => notice.set(crate::i18n::message(error)),
            }
        });
    };
    let assign = Callback::new(move |(account_id, huddle): (AccountId, u8)| {
        run(HuddleChange::Assign { account_id, huddle });
    });
    on_cleanup(|| PLANNING.with(|p| p.set(false)));
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
        let opened = open.get();
        PLANNING.with(|p| p.set(opened));
        if !opened {
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
            // The planner is at most 520 px wide (style.css); keep it on screen,
            // and above the message box rather than half over it.
            let left = (dock.right() + 8.0).min(width - 528.0).max(8.0);
            let composer = document()
                .query_selector(".composer-area")
                .ok()
                .flatten()
                .map(|c| c.get_bounding_client_rect())
                .filter(|c| c.width() > 0.0 && c.left() < left + 520.0 && c.right() > left);
            let bottom = composer.map_or(dock.bottom(), |c| dock.bottom().min(c.top() - 8.0));
            position.set((left, (height - bottom).max(8.0)));
        }
        // Move focus into the planner: the first person, else the dialog.
        request_animation_frame(move || {
            if let Some(dialog) = dialog.get_untracked() {
                let first = dialog
                    .query_selector(".huddle-person")
                    .ok()
                    .flatten()
                    .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok());
                match first {
                    Some(first) => {
                        let _ = first.focus();
                    }
                    None => {
                        let _ = dialog.focus();
                    }
                }
            }
        });
        leptos::task::spawn_local(async move {
            if let Ok(status) = ui
                .audio_control
                .invoke::<thiscord_shared::audio::AudioShortcutStatus>("audio_shortcuts", json!({}))
                .await
                && !shortcut.is_disposed()
            {
                shortcut.set(status.shortcuts.huddles.clone());
                saved_shortcuts.set(Some(status.shortcuts));
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
                <div node_ref=dialog tabindex="-1" class="huddle-popover" role="dialog" aria-labelledby="huddle-title" aria-describedby="huddle-help huddle-keys"
                    style:left=move || format!("{}px", position.get().0) style:bottom=move || format!("{}px", position.get().1)>
                    <div class="huddle-head">
                        <h2 id="huddle-title" class="soundboard-title !mb-0"><Icon kind=Glyph::Grid/><span>{move || crate::i18n::t("Huddles")}</span></h2>
                        <span class="huddle-state" class:split=move || split.get()>{move || crate::i18n::t(if split.get() { "Split" } else { "Together" })}</span>
                    </div>
                    <p id="huddle-keys" class="sr-only">{move || crate::i18n::t("Press 1 to 4 for a huddle or 0 for the main room, or press Enter and choose Move here on a lane. Arrow keys move between people.")}</p>
                    <p id="huddle-help" class="huddle-help">{move || crate::i18n::t("Drag people into huddles, or select someone and press 1–4. When split, each huddle hears only itself; screens and cameras stay visible.")}</p>
                    <Lane ui huddle=0 members picked refocus assign/>
                    <div class="huddle-grid">
                        {(1..=MAX_HUDDLES).map(|huddle| view! {<Lane ui huddle members picked refocus assign/>}).collect_view()}
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
                    <Show when=move || recording.get() fallback=move || view! {
                        <p class="huddle-shortcut">
                            {move || {
                                let keys = thiscord_frontend::web_shortcuts::labels(&shortcut.get());
                                if keys.is_empty() {
                                    view! {<span>{move || crate::i18n::t("No shortcut yet.")}</span>}.into_any()
                                } else {
                                    view! {<span class="flex flex-wrap items-center gap-1">{move || crate::i18n::t("Shortcut:")}{keys.into_iter().map(|k| view! {<kbd>{k}</kbd>}).collect_view()}</span>}.into_any()
                                }
                            }}
                            <button type="button" class="huddle-link" disabled=move || saved_shortcuts.with(Option::is_none)
                                on:click=move |_| { shortcut_message.set(String::new()); recording.set(true); }>
                                {move || crate::i18n::t(if shortcut.with(String::is_empty) { "Set a shortcut" } else { "Change" })}
                            </button>
                        </p>
                    }>
                        <div class="huddle-shortcut-edit">
                            <super::shortcuts::KeyRecorder label="Split or regroup huddles" value=shortcut busy=shortcut_busy dirty=shortcut_dirty message=shortcut_message/>
                            <div class="huddle-actions">
                                <button type="button" class="settings-primary" disabled=move || shortcut_busy.get() || !shortcut_dirty.get() on:click=move |_| save_shortcut()>{move || crate::i18n::t("Save shortcut")}</button>
                                <button type="button" class="settings-secondary" disabled=move || shortcut_busy.get() on:click=move |_| {
                                    recording.set(false);
                                    shortcut_dirty.set(false);
                                    shortcut_message.set(String::new());
                                    shortcut.set(saved_shortcuts.get_untracked().map(|s| s.huddles).unwrap_or_default());
                                }>{move || crate::i18n::t("Cancel")}</button>
                            </div>
                            <Show when=move || !shortcut_message.get().is_empty()><p class="soundboard-notice" role="alert">{move || crate::i18n::message(shortcut_message.get())}</p></Show>
                        </div>
                    </Show>
                    <Show when=move || !notice.get().is_empty()><p class="soundboard-notice" role="alert">{move || notice.get()}</p></Show>
                    <p class="sr-only" aria-live="polite">{move || announce.get()}</p>
                </div>
            </Show>
        </div>
    </Show>}
}

/// Tells everyone in a split call which huddle they are in, who hears them and
/// who last changed that. Real changes (split, regroup, being moved while
/// split) play a distinct cue, so someone in a full-screen game hears them, and
/// are announced to screen readers; joining a split room is described without
/// a cue, and a reconnect is no change (`thiscord_frontend::huddles::transition`).
#[component]
pub(super) fn HuddleStatus(ui: Ui) -> impl IntoView {
    let line = Memo::new(move |_| {
        let members = participants(ui);
        let (huddle, others) = logic::companions(&members, own(ui)?)?;
        Some((
            huddle,
            others.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            state_of(&members).split_by,
        ))
    });
    let by = move |actor: Option<AccountId>| {
        actor
            .filter(|a| Some(*a) != own(ui))
            .map(|a| display(ui, None, a))
    };
    // `None` while reconnecting or outside a server call: keep the last view.
    let view = Memo::new(move |_| {
        if ui.voice.with(|v| v.channel_id.is_none()) {
            return Some(None);
        }
        let (_, channel) = call(ui)?;
        let me = own(ui)?;
        let members = participants(ui);
        let mine = members.iter().find(|m| m.account_id == me)?;
        Some(Some(logic::View {
            channel,
            split: mine.split,
            huddle: mine.huddle,
        }))
    });
    let last = StoredValue::new(None::<logic::View>);
    let announce = RwSignal::new(String::new());
    Effect::new(move |_| {
        let Some(next) = view.get() else {
            return;
        };
        let event = logic::transition(last.get_value().as_ref(), next.as_ref());
        last.set_value(next);
        let Some(event) = event else {
            return;
        };
        let lang = crate::i18n::language();
        let current = line.get_untracked();
        let actor = untrack(|| state_of(&participants(ui)).split_by);
        let lead = match (event, by(actor)) {
            (logic::Event::Joined, _) => crate::i18n::t("This room is split.").to_owned(),
            (logic::Event::Split, Some(name)) => {
                thiscord_frontend::localized_format!(lang, "{} split the room.", name)
            }
            (logic::Event::Split, None) => crate::i18n::t("The room is split.").to_owned(),
            (logic::Event::Regrouped, Some(name)) => thiscord_frontend::localized_format!(
                lang,
                "{} brought everyone back together.",
                name
            ),
            (logic::Event::Regrouped, None) => {
                crate::i18n::t("Everyone is back together").to_owned()
            }
            (logic::Event::Moved, Some(name)) => {
                thiscord_frontend::localized_format!(lang, "{} moved you to another huddle.", name)
            }
            (logic::Event::Moved, None) => {
                crate::i18n::t("You moved to another huddle.").to_owned()
            }
        };
        announce.set(match current {
            Some((huddle, others, _)) if !others.is_empty() => format!(
                "{lead} {}",
                thiscord_frontend::localized_format!(
                    lang,
                    "You are in {} with {}.",
                    name(huddle),
                    others.join(", ")
                )
            ),
            Some((huddle, _, _)) => format!(
                "{lead} {}",
                thiscord_frontend::localized_format!(lang, "You are alone in {}.", name(huddle))
            ),
            None => lead,
        });
        match event {
            logic::Event::Joined => {}
            logic::Event::Split => cue(ui, Cue::Split),
            logic::Event::Regrouped => cue(ui, Cue::Regroup),
            logic::Event::Moved => cue(ui, Cue::Moved),
        }
    });
    view! {<p class="sr-only" aria-live="polite">{move || announce.get()}</p><Show when=move || line.with(Option::is_some)>
        <p class=move || format!("huddle-status huddle-{}", line.get().map_or(0, |l| l.0))
            data-tip=move || line.get().and_then(|(_, _, actor)| by(actor)).map(|name| thiscord_frontend::localized_format!(crate::i18n::language(), "Changed by {}", name))>
            <span class="huddle-dot" aria-hidden="true"></span>
            <span class="min-w-0 truncate">{move || line.get().map(|(huddle, others, _)| if others.is_empty() {
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
