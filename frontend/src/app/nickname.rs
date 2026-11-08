//! Per-server nickname dialog, for yourself or (with Manage nicknames) a
//! lower-ranked member. Opened from member menus, the server menu and profile
//! cards through `ui.members.nickname`.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::{
    AccountId, GuildId,
    account::valid_display_name,
    chat::{CHAT_PATH, ChatRequest, ChatResponse, MemberProfile},
    permissions::{Guild, GuildChange, MAX_NICKNAME_CHARS, PermissionRequest, PermissionResponse},
};

/// What the dialog is editing: the guild (with the revision the change will
/// submit) and the member's current profile and nickname.
#[derive(Clone)]
struct Loaded {
    guild: Guild,
    profile: MemberProfile,
}

/// Opens the dialog for `account` in `guild`.
pub(super) fn open(ui: Ui, guild: GuildId, account: AccountId) {
    ui.members.nickname.set(Some((guild, account)));
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let input = NodeRef::<leptos::html::Input>::new();
    let loaded = RwSignal::new(None::<Loaded>);
    let value = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let generation = RwSignal::new(0_u64);
    let target = move || ui.members.nickname.get();
    let own = move || {
        target().is_some_and(|(_, id)| ui.account.with(|a| a.as_ref().is_some_and(|a| a.id == id)))
    };
    // Reads the current revision and nickname. After a rejected save it keeps
    // what was typed; nothing is ever retried automatically.
    let load = move |guild: GuildId, account: AccountId, keep_typed: bool| {
        let current = generation.get_untracked().wrapping_add(1);
        generation.set(current);
        let token = ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let home =
                super::servers::request(ui, PermissionRequest::ViewGuild { guild_id: guild }).await;
            let profile = crate::account_client::api_request::<_, ChatResponse>(
                CHAT_PATH,
                &ChatRequest::MemberProfile {
                    guild_id: guild,
                    account_id: account,
                },
                token.as_deref(),
            )
            .await;
            if generation.try_get_untracked() != Some(current) || ui.token.get_untracked() != token
            {
                return;
            }
            match (home, profile) {
                (
                    Ok(PermissionResponse::Home { home }),
                    Ok(ChatResponse::MemberProfile { profile }),
                ) => {
                    if !keep_typed {
                        value.set(profile.nickname.clone().unwrap_or_default());
                    }
                    loaded.set(Some(Loaded {
                        guild: home.guild,
                        profile,
                    }));
                    if !keep_typed {
                        // After the loaded value renders into the field.
                        request_animation_frame(move || {
                            if let Some(input) = input.get_untracked() {
                                let _ = input.focus();
                                input.select();
                            }
                        });
                    }
                }
                (Err(e), _) | (_, Err(e)) => error.set(e),
                _ => error.set("Unexpected server response".into()),
            }
        });
    };
    Effect::new(move |_| {
        let target = ui.members.nickname.get();
        generation.update(|g| *g = g.wrapping_add(1));
        loaded.set(None);
        error.set(String::new());
        saving.set(false);
        value.set(String::new());
        let Some((guild, account)) = target else {
            if let Some(dialog) = dialog.get_untracked() {
                dialog.close();
            }
            return;
        };
        if let Some(dialog) = dialog.get_untracked() {
            super::dialog::open(&dialog);
        }
        load(guild, account, false);
    });
    // Signing out or switching accounts closes it.
    Effect::new(move |previous: Option<Option<String>>| {
        let token = ui.token.get();
        if previous.is_some_and(|p| p != token) {
            ui.members.nickname.set(None);
        }
        token
    });
    let current = move || {
        loaded.with(|l| {
            l.as_ref()
                .and_then(|l| l.profile.nickname.clone())
                .unwrap_or_default()
        })
    };
    let trimmed = move || value.with(|v| v.trim().to_owned());
    let invalid = move || {
        let typed = value.get();
        !typed.trim().is_empty() && !valid_display_name(&typed)
    };
    let unchanged = move || trimmed() == current();
    let save = move |nickname: Option<String>| {
        let (Some((guild_id, account_id)), Some(state)) =
            (ui.members.nickname.get_untracked(), loaded.get_untracked())
        else {
            return;
        };
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        error.set(String::new());
        let reset = nickname.is_none();
        let token = ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let result = super::servers::request(
                ui,
                PermissionRequest::Change {
                    guild_id,
                    revision: state.guild.revision,
                    change: GuildChange::SetNickname {
                        account_id,
                        nickname,
                    },
                },
            )
            .await;
            if ui.token.get_untracked() != token
                || ui.members.nickname.get_untracked() != Some((guild_id, account_id))
            {
                return;
            }
            saving.set(false);
            match result {
                Ok(_) => {
                    ui.status.set(
                        if reset {
                            "Nickname reset"
                        } else {
                            "Nickname saved"
                        }
                        .into(),
                    );
                    ui.members.nickname.set(None);
                }
                Err(e) => {
                    // Show the reason and reload the server's current state.
                    error.set(e);
                    load(guild_id, account_id, true);
                }
            }
        });
    };
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if invalid() || unchanged() {
            return;
        }
        let nickname = trimmed();
        save((!nickname.is_empty()).then_some(nickname));
    };
    let server =
        move || loaded.with(|l| l.as_ref().map(|l| l.guild.name.clone()).unwrap_or_default());
    let profile_name = move || {
        loaded.with(|l| {
            l.as_ref()
                .map(|l| {
                    if l.profile.display_name.trim().is_empty() {
                        l.profile.username.clone()
                    } else {
                        l.profile.display_name.clone()
                    }
                })
                .unwrap_or_default()
        })
    };
    view! {
        <dialog node_ref=dialog class="context-dialog nickname-dialog" aria-labelledby="nickname-title" aria-describedby="nickname-hint"
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, saving.get_untracked())
            on:cancel=move |ev: web_sys::Event| if saving.get_untracked() { ev.prevent_default(); }
            on:close=move |_| ui.members.nickname.set(None)>
            <header class="mb-4 flex items-start justify-between gap-3">
                <div class="min-w-0">
                    <h2 id="nickname-title" class="text-xl font-semibold">{move || crate::i18n::t("Change nickname")}</h2>
                    <p class="truncate text-sm text-white/60">{move || loaded.with(|l| l.as_ref().map(|l| format!("@{} · {}", l.profile.username, l.guild.name)))}</p>
                </div>
                <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Close dialog") disabled=move || saving.get()
                    on:click=move |_| if let Some(d) = dialog.get() { d.close(); }><Icon kind=Glyph::Close/></button>
            </header>
            <form class="space-y-4" on:submit=submit>
                <label class="settings-field">
                    <span>{move || crate::i18n::t("Nickname")}</span>
                    <input node_ref=input class="w-full rounded-md border border-white/20 bg-black/20 px-3 py-2 focus:border-brand focus:outline-none"
                        autocomplete="off" spellcheck="false" maxlength=(MAX_NICKNAME_CHARS * 2).to_string()
                        placeholder=profile_name aria-invalid=move || invalid().to_string()
                        autofocus readonly=move || loaded.with(Option::is_none) || saving.get()
                        prop:value=move || value.get() on:input=move |e| value.set(event_target_value(&e))/>
                    <small aria-live="polite">{move || format!("{}/{MAX_NICKNAME_CHARS}", value.with(|v| v.trim().chars().count()))}</small>
                </label>
                <p id="nickname-hint" class="text-xs text-white/60">{move || if own() {
                    thiscord_frontend::localized_format!(crate::i18n::language(), "Shown instead of your profile name in {server} only. Leave it empty to use {name}.", server = server(), name = profile_name())
                } else {
                    thiscord_frontend::localized_format!(crate::i18n::language(), "Shown instead of their profile name in {server} only. Leave it empty to use {name}. Moderator changes are recorded in the audit log.", server = server(), name = profile_name())
                }}</p>
                <Show when=invalid>
                    <p class="text-sm text-red-300" role="alert">{move || crate::i18n::t("Nickname must be 1 to 64 characters without control characters")}</p>
                </Show>
                <Show when=move || loaded.with(Option::is_none) && error.with(String::is_empty)>
                    <p class="text-sm text-white/60" role="status">{move || crate::i18n::t("Loading…")}</p>
                </Show>
                <p class="text-sm text-red-300" role="alert" hidden=move || error.with(String::is_empty)>{move || crate::i18n::message(error.get())}</p>
                <div class="flex flex-wrap justify-end gap-2">
                    <Show when=move || !current().is_empty()>
                        <button type="button" class="mr-auto rounded bg-white/10 px-4 py-2 text-sm" disabled=move || saving.get()
                            on:click=move |_| save(None)>{move || crate::i18n::t("Reset nickname")}</button>
                    </Show>
                    <button type="button" class="rounded bg-white/10 px-4 py-2 text-sm" disabled=move || saving.get()
                        on:click=move |_| if let Some(d) = dialog.get() { d.close(); }>{move || crate::i18n::t("Cancel")}</button>
                    <button class="rounded bg-brand px-4 py-2 text-sm font-semibold disabled:opacity-50"
                        disabled=move || saving.get() || loaded.with(Option::is_none) || invalid() || unchanged()>
                        {move || if saving.get() { crate::i18n::t("Saving…") } else { crate::i18n::t("Save") }}</button>
                </div>
            </form>
        </dialog>
    }
}
