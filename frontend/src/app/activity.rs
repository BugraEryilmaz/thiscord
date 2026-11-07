//! Detected game activity ("Playing …"): desktop detection state, the device
//! setting and display labels. The friends socket shares it (see `friends`).
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_frontend::game_activity::{
    ACTIVITY_EVENT, ActivityStatus, CustomGame, RunningProgram, builtin_names, process_key,
    suggested_name,
};
use thiscord_shared::account::{Activity, ActivityKind};

/// Plain-text label ("Playing Factorio"); never markup.
pub(super) fn label(activity: &Activity) -> String {
    match activity.kind {
        ActivityKind::Playing => thiscord_frontend::localized_format!(
            crate::i18n::language(),
            "Playing {}",
            activity.name
        ),
    }
}

/// This device's shared activity, for the account ribbon.
pub(super) fn own(ui: Ui) -> Option<String> {
    ui.activity.with(|s| s.activity.as_ref().map(label))
}

/// Follows native detection for the lifetime of the app (desktop only).
#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let listener = StoredValue::new_local(None::<crate::account_client::EventListener>);
    on_cleanup(move || listener.set_value(None));
    if desktop() {
        leptos::task::spawn_local(async move {
            let Ok(handle) =
                crate::account_client::listen(ACTIVITY_EVENT, move |status: ActivityStatus| {
                    if !ui.activity.is_disposed() {
                        ui.activity.set(status);
                    }
                })
                .await
            else {
                return;
            };
            if listener.is_disposed() {
                return;
            }
            listener.set_value(Some(handle));
            if let Ok(status) = native::<ActivityStatus>("activity_status", json!({})).await
                && !ui.activity.is_disposed()
            {
                ui.activity.set(status);
            }
        });
    }
}

/// Saves the added and hidden games; errors stay in `error`.
fn save_games(
    ui: Ui,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    games: Vec<CustomGame>,
    hidden: Vec<String>,
    done: impl FnOnce() + 'static,
) {
    if busy.get_untracked() {
        return;
    }
    busy.set(true);
    error.set(String::new());
    leptos::task::spawn_local(async move {
        let result = native::<ActivityStatus>(
            "activity_set_games",
            json!({"games": games, "hidden": hidden}),
        )
        .await;
        if busy.try_set(false).is_some() {
            return;
        }
        match result {
            Ok(status) => {
                ui.activity.set(status);
                done();
            }
            Err(message) => error.set(message),
        }
    });
}

/// First letter of a game, for its tile.
fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".into())
}

#[component]
pub(super) fn Settings(ui: Ui) -> impl IntoView {
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    // The detected game, and whether it is a built-in one that can be hidden.
    let detected = Memo::new(move |_| {
        ui.activity.with(|s| {
            s.activity.as_ref().map(|a| {
                let builtin = !s.games.iter().any(|g| g.name == a.name)
                    && builtin_names().any(|name| name == a.name);
                (a.name.clone(), builtin)
            })
        })
    });
    let hide = move |name: String| {
        let (games, mut hidden) = ui
            .activity
            .with_untracked(|s| (s.games.clone(), s.hidden.clone()));
        hidden.push(name.clone());
        save_games(ui, busy, error, games, hidden, move || {
            ui.status.set(thiscord_frontend::localized_format!(
                crate::i18n::language(),
                "{} won't be shown as your activity on this computer",
                name
            ))
        });
    };
    let show = move |name: String| {
        let (games, mut hidden) = ui
            .activity
            .with_untracked(|s| (s.games.clone(), s.hidden.clone()));
        hidden.retain(|h| *h != name);
        save_games(ui, busy, error, games, hidden, || {});
    };
    let remove = move |process: String| {
        let (mut games, hidden) = ui
            .activity
            .with_untracked(|s| (s.games.clone(), s.hidden.clone()));
        games.retain(|g| g.process != process);
        save_games(ui, busy, error, games, hidden, || {});
    };
    view! {
        <div class="settings-card">
            <h3>{move || crate::i18n::t("Activity status")}</h3>
            <label class="flex items-center gap-3">
                <input type="checkbox" prop:checked=move || ui.activity.with(|s| s.enabled)
                    disabled=move || busy.get()
                    on:change=move |ev| {
                        if busy.get_untracked() { return; }
                        let previous = ui.activity.get_untracked();
                        let value = event_target_checked(&ev);
                        ui.activity.update(|s| { s.enabled = value; if !value { s.activity = None; } });
                        busy.set(true);
                        error.set(String::new());
                        leptos::task::spawn_local(async move {
                            let result = native::<ActivityStatus>("activity_set_enabled", json!({"enabled":value})).await;
                            if busy.try_get_untracked().is_none() { return; }
                            match result {
                                Ok(status) => ui.activity.set(status),
                                Err(message) => { ui.activity.set(previous); error.set(message); }
                            }
                            busy.set(false);
                        });
                    }/>
                {move || crate::i18n::t("Show the game I'm playing as my status")}
            </label>
            <p class="settings-hint">{move || crate::i18n::t("Thiscord looks for known games among the programs running on this computer. Only the game's name is shared, with friends and members of your servers, while Thiscord is open.")}</p>
            <Show when=move || ui.activity.with(|s| s.enabled)>
                <div class="game-now" role="status">
                    {move || match detected.get() {
                        Some((name, builtin)) => view! {
                            <span class="game-tile" aria-hidden="true">{initial(&name)}</span>
                            <div class="min-w-0 flex-1">
                                <span class="game-now-label">{move || crate::i18n::t("Detected now")}</span>
                                <strong>{thiscord_frontend::localized_format!(crate::i18n::language(), "Playing {}", name.clone())}</strong>
                            </div>
                            <Show when=move || builtin>
                                {
                                    let name = name.clone();
                                    view! {
                                        <button type="button" class="settings-secondary" disabled=move || busy.get()
                                            on:click=move |_| hide(name.clone())>
                                            <Icon kind=Glyph::EyeOff/><span>{move || crate::i18n::t("Hide this game")}</span>
                                        </button>
                                    }
                                }
                            </Show>
                        }.into_any(),
                        None => view! {
                            <span class="game-tile game-tile-idle" aria-hidden="true"><Icon kind=Glyph::Search/></span>
                            <div class="min-w-0 flex-1">
                                <span class="game-now-label">{move || crate::i18n::t("Detected now")}</span>
                                <strong>{move || crate::i18n::t("No game detected right now.")}</strong>
                            </div>
                        }.into_any(),
                    }}
                </div>
            </Show>
            <p class="settings-hint">{move || crate::i18n::t("On by default. Changes save automatically on this computer.")}</p>
        </div>
        <section class="settings-card" aria-labelledby="added-games-title">
            <div class="game-card-head">
                <div class="min-w-0 flex-1">
                    <h3 id="added-games-title">{move || crate::i18n::t("Your games")}</h3>
                    <p class="settings-hint">{move || crate::i18n::t("Add games Thiscord doesn't recognize, or give one a different name. They're saved on this computer only.")}</p>
                </div>
                <Show when=move || !adding.get()>
                    <button type="button" class="settings-primary" disabled=move || busy.get()
                        on:click=move |_| { error.set(String::new()); adding.set(true); }>
                        <Icon kind=Glyph::Plus/><span>{move || crate::i18n::t("Add a game")}</span>
                    </button>
                </Show>
            </div>
            <Show when=move || adding.get()>
                <AddGame ui=ui busy=busy error=error adding=adding/>
            </Show>
            <Show when=move || ui.activity.with(|s| !s.games.is_empty()) fallback=move || view! {
                <Show when=move || !adding.get()><p class="game-empty">{move || crate::i18n::t("No added games yet.")}</p></Show>
            }>
                <ul class="game-list">
                    <For each=move || ui.activity.with(|s| s.games.clone()) key=|game| (game.process.clone(), game.name.clone()) let:game>
                        {
                            let process = game.process.clone();
                            let label = game.name.clone();
                            view! {
                                <li class="game-row">
                                    <span class="game-tile" aria-hidden="true">{initial(&game.name)}</span>
                                    <div class="min-w-0 flex-1">
                                        <strong>{game.name.clone()}</strong>
                                        <code class="game-process">{game.process.clone()}</code>
                                    </div>
                                    <button type="button" class="game-remove" disabled=move || busy.get()
                                        aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "Remove {}", label.clone())
                                        data-tip=move || crate::i18n::t("Remove")
                                        on:click=move |_| remove(process.clone())>
                                        <Icon kind=Glyph::Trash/>
                                    </button>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </Show>
            <p class="identity-error" role="alert">{move || crate::i18n::message(error.get())}</p>
        </section>
        <Show when=move || ui.activity.with(|s| !s.hidden.is_empty())>
            <section class="settings-card" aria-labelledby="hidden-games-title">
                <h3 id="hidden-games-title">{move || crate::i18n::t("Hidden games")}</h3>
                <p class="settings-hint">{move || crate::i18n::t("These games are never shown as your activity on this computer.")}</p>
                <ul class="game-list">
                    <For each=move || ui.activity.with(|s| s.hidden.clone()) key=|name| name.clone() let:name>
                        {
                            let shown = name.clone();
                            view! {
                                <li class="game-row">
                                    <span class="game-tile game-tile-hidden" aria-hidden="true"><Icon kind=Glyph::EyeOff/></span>
                                    <div class="min-w-0 flex-1"><strong>{name.clone()}</strong></div>
                                    <button type="button" class="settings-link" disabled=move || busy.get()
                                        on:click=move |_| show(shown.clone())>{move || crate::i18n::t("Show again")}</button>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </section>
        </Show>
    }
}

/// Choosing a running program (or typing its name) and naming the game.
#[component]
fn AddGame(
    ui: Ui,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    adding: RwSignal<bool>,
) -> impl IntoView {
    let programs = RwSignal::new(None::<Result<Vec<RunningProgram>, String>>);
    let query = RwSignal::new(String::new());
    let process = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    // Until the name is typed, it follows the chosen program.
    let named = RwSignal::new(false);
    let search = NodeRef::<leptos::html::Input>::new();
    let load = move || {
        programs.set(None);
        leptos::task::spawn_local(async move {
            let result = native::<Vec<RunningProgram>>("activity_programs", json!({})).await;
            let _ = programs.try_set(Some(result));
        });
    };
    load();
    Effect::new(move |_| {
        if let Some(input) = search.get() {
            let _ = input.focus();
        }
    });
    let added = move |key: &str| {
        ui.activity
            .with(|s| s.games.iter().any(|g| g.process == key))
    };
    let visible = Memo::new(move |_| {
        let query = query.get().trim().to_lowercase();
        programs.with(|p| match p {
            Some(Ok(list)) => list
                .iter()
                .filter(|p| {
                    query.is_empty()
                        || p.name.to_lowercase().contains(&query)
                        || p.process.contains(&query)
                })
                .cloned()
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
    });
    let choose = move |program: RunningProgram| {
        process.set(program.process);
        if !named.get_untracked() {
            name.set(program.name);
        }
    };
    let key = Memo::new(move |_| process_key(&process.get()));
    let duplicate = move || added(&key.get());
    let ready = move || !key.get().is_empty() && !name.get().trim().is_empty() && !duplicate();
    let close = move || {
        adding.set(false);
        error.set(String::new());
    };
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if !ready() {
            return;
        }
        let game = CustomGame {
            name: name.get_untracked().trim().to_owned(),
            process: key.get_untracked(),
        };
        let label = game.name.clone();
        let (mut games, hidden) = ui
            .activity
            .with_untracked(|s| (s.games.clone(), s.hidden.clone()));
        games.push(game);
        save_games(ui, busy, error, games, hidden, move || {
            adding.set(false);
            ui.status.set(thiscord_frontend::localized_format!(
                crate::i18n::language(),
                "{} added to your games",
                label
            ));
        });
    };
    view! {
        <form class="game-picker" on:submit=submit
            on:keydown=move |event: leptos::ev::KeyboardEvent| {
                // Escape closes the picker, not the whole settings window.
                if event.key() == "Escape" && !busy.get_untracked() {
                    event.prevent_default();
                    event.stop_propagation();
                    close();
                }
            }>
            <div class="game-picker-head">
                <strong>{move || crate::i18n::t("Choose a running program")}</strong>
                <button type="button" class="game-refresh" disabled=move || programs.with(Option::is_none)
                    aria-label=move || crate::i18n::t("Refresh the list") data-tip=move || crate::i18n::t("Refresh the list")
                    on:click=move |_| load()><Icon kind=Glyph::Refresh/></button>
            </div>
            <label class="game-search">
                <Icon kind=Glyph::Search/>
                <span class="sr-only">{move || crate::i18n::t("Search running programs")}</span>
                <input node_ref=search type="search" spellcheck="false" autocomplete="off"
                    placeholder=move || crate::i18n::t("Search running programs")
                    prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e))/>
            </label>
            <div class="game-picker-list" role="listbox" aria-label=move || crate::i18n::t("Running programs")>
                {move || match programs.get() {
                    None => view! {<p class="game-picker-note">{move || crate::i18n::t("Looking for running programs…")}</p>}.into_any(),
                    Some(Err(message)) => view! {<p class="game-picker-note" role="alert">{crate::i18n::message(message)}</p>}.into_any(),
                    Some(Ok(_)) if visible.with(Vec::is_empty) => view! {<p class="game-picker-note">{move || crate::i18n::t("No matching programs. Start the game, then refresh, or type its program name below.")}</p>}.into_any(),
                    Some(Ok(_)) => view! {
                        <For each=move || visible.get() key=|p| p.process.clone() let:program>
                            {
                                let key = program.process.clone();
                                let selected = {
                                    let key = key.clone();
                                    move || process_key(&process.get()) == key
                                };
                                let already = {
                                    let key = key.clone();
                                    move || added(&key)
                                };
                                let tile = initial(&program.name);
                                view! {
                                    <button type="button" role="option" class="game-option" aria-selected=move || selected().to_string()
                                        disabled=already.clone()
                                        on:click={
                                            let program = program.clone();
                                            move |_| choose(program.clone())
                                        }>
                                        <span class="game-tile" aria-hidden="true">{tile}</span>
                                        <span class="min-w-0 flex-1">
                                            <strong>{program.name.clone()}</strong>
                                            <code class="game-process">{key.clone()}</code>
                                        </span>
                                        <Show when=already.clone()><span class="settings-badge">{move || crate::i18n::t("Added")}</span></Show>
                                    </button>
                                }
                            }
                        </For>
                    }.into_any(),
                }}
            </div>
            <div class="game-picker-fields">
                <label class="field">
                    <span class="field-label">{move || crate::i18n::t("Program")}</span>
                    <input class="field-input game-process-input" type="text" spellcheck="false" autocomplete="off" maxlength="160"
                        placeholder=move || crate::i18n::t("game.exe")
                        prop:value=move || process.get()
                        on:input=move |e| {
                            let value = event_target_value(&e);
                            if !named.get_untracked() {
                                name.set(suggested_name(&value));
                            }
                            process.set(value);
                        }/>
                </label>
                <label class="field">
                    <span class="field-label">{move || crate::i18n::t("Shown as")}</span>
                    <input class="field-input" type="text" maxlength="64" autocomplete="off"
                        placeholder=move || crate::i18n::t("Game name")
                        prop:value=move || name.get()
                        on:input=move |e| { named.set(true); name.set(event_target_value(&e)); }/>
                </label>
            </div>
            <p class="settings-hint">{move || if name.get().trim().is_empty() {
                crate::i18n::t("Pick a program above, or type the name of the game's program file.").to_owned()
            } else {
                thiscord_frontend::localized_format!(crate::i18n::language(), "Friends will see “Playing {}”.", name.get().trim())
            }}</p>
            <Show when=duplicate><p class="identity-error" role="alert">{move || crate::i18n::t("That program is already in your games")}</p></Show>
            <div class="settings-actions">
                <button type="button" class="settings-secondary" disabled=move || busy.get() on:click=move |_| close()>{move || crate::i18n::t("Cancel")}</button>
                <button type="submit" class="settings-primary" disabled=move || busy.get() || !ready()>{move || crate::i18n::t("Add game")}</button>
            </div>
        </form>
    }
}
