//! Settings → Profile → Account details: changing the username and email.
//! Both need recent reauthentication; a verified email changes only once the
//! link sent to the new address is opened (docs/accounts.md).
use super::{
    Ui, client,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::account::{
    Account, AccountRequest, AccountResponse, GooglePurpose, IdentityProvider,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Editing {
    Username,
    Email,
}

/// Sends `commands` in order and applies the account the last one returns.
/// Never applies an old session's result to a newly signed-in account.
async fn send(ui: Ui, commands: Vec<AccountRequest>) -> Result<Option<Account>, String> {
    let Some(token) = ui.token.get_untracked() else {
        return Err("Sign in again to continue".into());
    };
    let mut last = None;
    for command in commands {
        last = Some(client::request(&command, Some(&token)).await);
        if matches!(last, Some(Err(_))) {
            break;
        }
    }
    if ui.token.get_untracked().as_deref() != Some(&token) {
        return Ok(None);
    }
    match last {
        Some(Ok(AccountResponse::Account { account })) => {
            ui.account.set(Some(account.clone()));
            Ok(Some(account))
        }
        Some(Ok(_)) | None => Ok(None),
        Some(Err(error)) => Err(error),
    }
}

/// Runs an identity command under the shared busy flag. `done` receives the
/// updated account; errors stay inline in `error`.
fn perform(
    ui: Ui,
    error: RwSignal<String>,
    commands: Vec<AccountRequest>,
    done: impl FnOnce(Option<Account>) + 'static,
) {
    if ui.busy.get_untracked() {
        return;
    }
    ui.busy.set(true);
    error.set(String::new());
    leptos::task::spawn_local(async move {
        let result = send(ui, commands).await;
        ui.busy.set(false);
        match result {
            Ok(account) => done(account),
            Err(message) => {
                let _ = error.try_set(message);
            }
        }
    });
}

#[component]
pub(super) fn AccountDetails(ui: Ui) -> impl IntoView {
    let editing = RwSignal::new(None::<Editing>);
    let pending = Memo::new(move |_| {
        ui.account
            .with(|a| a.as_ref().and_then(|a| a.pending_email.clone()))
    });
    let pending_error = RwSignal::new(String::new());
    // The link is usually opened in a browser: catch up when Thiscord regains focus.
    let checking = StoredValue::new(false);
    let focus = window_event_listener(leptos::ev::focus, move |_| {
        if pending.get_untracked().is_none() || checking.get_value() || ui.busy.get_untracked() {
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        checking.set_value(true);
        leptos::task::spawn_local(async move {
            let result = client::request(&AccountRequest::Current, Some(&token)).await;
            if checking.try_set_value(false).is_some()
                || ui.token.get_untracked().as_deref() != Some(&token)
            {
                return;
            }
            if let Ok(AccountResponse::Account { account }) = result {
                let changed = ui
                    .account
                    .with_untracked(|a| a.as_ref().is_some_and(|a| a.email != account.email));
                if changed {
                    ui.status.set(thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "Your email address is now {}",
                        account.email
                    ));
                }
                ui.account.set(Some(account));
            }
        });
    });
    on_cleanup(move || focus.remove());
    let toggle = move |which: Editing| {
        editing.update(|current| {
            *current = if *current == Some(which) {
                None
            } else {
                Some(which)
            }
        })
    };
    let resend = move |_| {
        let address = pending.get_untracked().unwrap_or_default();
        perform(
            ui,
            pending_error,
            vec![AccountRequest::SendVerification],
            move |_| {
                ui.status.set(thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "We sent a new link to {}",
                    address
                ))
            },
        );
    };
    let cancel = move |_| {
        perform(
            ui,
            pending_error,
            vec![AccountRequest::CancelEmailChange],
            move |_| {
                ui.status
                    .set(crate::i18n::t("Email change cancelled").into())
            },
        );
    };
    view! {
        <section class="settings-card" aria-labelledby="account-details-title">
            <h3 id="account-details-title">{move || crate::i18n::t("Account details")}</h3>
            <div class="identity-list">
                <div class="identity-row">
                    <div class="identity-value">
                        <span>{move || crate::i18n::t("Username")}</span>
                        <strong>{move || ui.account.get().map(|a| format!("@{}", a.username))}</strong>
                    </div>
                    <button type="button" class="settings-secondary identity-edit" aria-expanded=move || (editing.get() == Some(Editing::Username)).to_string()
                        aria-controls="identity-username-editor" disabled=move || ui.busy.get()
                        on:click=move |_| toggle(Editing::Username)>
                        {move || if editing.get() == Some(Editing::Username) { view! {<Icon kind=Glyph::Close/>}.into_any() } else { view! {<Icon kind=Glyph::Edit/>}.into_any() }}<span>{move || if editing.get() == Some(Editing::Username) { crate::i18n::t("Close") } else { crate::i18n::t("Edit") }}</span>
                    </button>
                </div>
                <Show when=move || editing.get() == Some(Editing::Username)>
                    <Editor ui=ui kind=Editing::Username editing=editing/>
                </Show>
                <div class="identity-row">
                    <div class="identity-value">
                        <span>{move || crate::i18n::t("Email")}</span>
                        <strong>{move || ui.account.get().map(|a| a.email)}
                            <Show when=move || ui.account.with(|a| a.as_ref().is_some_and(|a| a.email_verified))>
                                <span class="settings-badge">{move || crate::i18n::t("Verified")}</span>
                            </Show>
                        </strong>
                    </div>
                    <button type="button" class="settings-secondary identity-edit" aria-expanded=move || (editing.get() == Some(Editing::Email)).to_string()
                        aria-controls="identity-email-editor" disabled=move || ui.busy.get()
                        on:click=move |_| toggle(Editing::Email)>
                        {move || if editing.get() == Some(Editing::Email) { view! {<Icon kind=Glyph::Close/>}.into_any() } else { view! {<Icon kind=Glyph::Edit/>}.into_any() }}<span>{move || if editing.get() == Some(Editing::Email) { crate::i18n::t("Close") } else { crate::i18n::t("Edit") }}</span>
                    </button>
                </div>
                <Show when=move || pending.get().is_some()>
                    <div class="identity-pending" role="status">
                        <span class="identity-pending-icon" aria-hidden="true"><Icon kind=Glyph::Send/></span>
                        <div class="min-w-0 flex-1">
                            <strong>{move || crate::i18n::t("Confirm your new email")}</strong>
                            <p>{move || thiscord_frontend::localized_format!(
                                crate::i18n::language(),
                                "We sent a link to {}. Your current email stays in use until you open it. Links expire after 30 minutes.",
                                pending.get().unwrap_or_default()
                            )}</p>
                            <p class="identity-error" role="alert">{move || crate::i18n::message(pending_error.get())}</p>
                            <div class="identity-pending-actions">
                                <button type="button" class="settings-link" disabled=move || ui.busy.get() on:click=resend>{move || crate::i18n::t("Resend link")}</button>
                                <button type="button" class="settings-link identity-cancel" disabled=move || ui.busy.get() on:click=cancel>{move || crate::i18n::t("Cancel change")}</button>
                            </div>
                        </div>
                    </div>
                </Show>
                <Show when=move || editing.get() == Some(Editing::Email)>
                    <Editor ui=ui kind=Editing::Email editing=editing/>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn Editor(ui: Ui, kind: Editing, editing: RwSignal<Option<Editing>>) -> impl IntoView {
    let value = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
        }
    });
    let has_password = move || {
        ui.account.with(|a| {
            a.as_ref()
                .is_some_and(|a| a.identities.contains(&IdentityProvider::Password))
        })
    };
    let username = kind == Editing::Username;
    let current = move || {
        ui.account.with(|a| {
            a.as_ref()
                .map(|a| {
                    if username {
                        a.username.clone()
                    } else {
                        a.email.clone()
                    }
                })
                .unwrap_or_default()
        })
    };
    let unchanged = move || {
        let next = value.get().trim().to_ascii_lowercase();
        next.is_empty() || next == current()
    };
    let close = move || editing.set(None);
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if unchanged() || (has_password() && password.get_untracked().is_empty()) {
            return;
        }
        let next = value.get_untracked().trim().to_owned();
        let mut commands = Vec::new();
        if has_password() {
            commands.push(AccountRequest::Reauthenticate {
                password: password.get_untracked(),
            });
        }
        password.set(String::new());
        commands.push(if username {
            AccountRequest::ChangeUsername { username: next }
        } else {
            AccountRequest::ChangeEmail { email: next }
        });
        perform(ui, error, commands, move |account| {
            let Some(account) = account else {
                return;
            };
            ui.status.set(if username {
                thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "Your username is now @{}",
                    account.username
                )
            } else if let Some(address) = account.pending_email {
                thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "Check {} for a link to confirm your new email",
                    address
                )
            } else {
                thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "We sent a new link to {}",
                    account.email
                )
            });
            let _ = editing.try_set(None);
        });
    };
    let id = if username {
        "identity-username-editor"
    } else {
        "identity-email-editor"
    };
    view! {
        <form id=id class="identity-editor" on:submit=submit
            on:keydown=move |event: leptos::ev::KeyboardEvent| {
                // Escape closes the editor, not the whole settings window.
                if event.key() == "Escape" && !ui.busy.get_untracked() {
                    event.prevent_default();
                    event.stop_propagation();
                    close();
                }
            }>
            <fieldset class="identity-fields" disabled=move || ui.busy.get()>
                <label class="field">
                    <span class="field-label">{move || if username { crate::i18n::t("New username") } else { crate::i18n::t("New email") }}</span>
                    <div class=if username { "identity-input identity-input-handle" } else { "identity-input" }>
                        <Show when=move || username><span aria-hidden="true">"@"</span></Show>
                        <input node_ref=input class="field-input" type=if username { "text" } else { "email" }
                            autocomplete=if username { "username" } else { "email" } spellcheck="false" autocapitalize="off"
                            maxlength=if username { "32" } else { "254" }
                            placeholder=current
                            prop:value=move || value.get() on:input=move |e| value.set(event_target_value(&e))/>
                    </div>
                </label>
                <p class="settings-hint">{move || if username {
                    crate::i18n::t("3 to 32 letters, numbers or underscores. You sign in with the new username, and friends and servers see it right away. Your old username becomes available to others.")
                } else {
                    crate::i18n::t("We’ll email a confirmation link to the new address. Your current email keeps working for sign-in and recovery until you open it.")
                }}</p>
                <Show when=has_password fallback=move || view! {
                    <div class="identity-google">
                        <p class="settings-hint">{move || crate::i18n::t("Your account signs in with Google. Confirm with Google, then save within five minutes.")}</p>
                        <button type="button" class="settings-secondary" on:click=move |_| super::google(ui, GooglePurpose::Reauthenticate)>{move || crate::i18n::t("Confirm with Google")}</button>
                    </div>
                }>
                    <label class="field">
                        <span class="field-label">{move || crate::i18n::t("Current password")}</span>
                        <input class="field-input" type="password" autocomplete="current-password"
                            prop:value=move || password.get() on:input=move |e| password.set(event_target_value(&e))/>
                    </label>
                </Show>
                <p class="identity-error" role="alert">{move || crate::i18n::message(error.get())}</p>
                <div class="settings-actions">
                    <button type="button" class="settings-secondary" on:click=move |_| close()>{move || crate::i18n::t("Cancel")}</button>
                    <button type="submit" class="settings-primary" disabled=move || unchanged() || (has_password() && password.get().is_empty())>
                        {move || if ui.busy.get() { crate::i18n::t("Saving…") } else if username { crate::i18n::t("Save username") } else { crate::i18n::t("Send confirmation link") }}
                    </button>
                </div>
            </fieldset>
        </form>
    }
}
