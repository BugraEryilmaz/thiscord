//! The pending-account step. Until the email is verified the app shows only this
//! screen: the account exists and is signed in, but servers, friends and voice
//! stay closed (the backend also refuses them, and hides pending accounts).
use super::{Field, Ui, apply, run};
use crate::account_client as client;
use leptos::prelude::*;
use std::time::Duration;
use thiscord_shared::account::{AccountRequest, AccountResponse};

/// Background checks while the screen is visible, in one-second ticks.
const CHECK_TICKS: u32 = 5;
const RESEND_COOLDOWN_MS: f64 = 60_000.0;

/// Reads the account without touching the shared busy flag; opens the app once
/// the link was used (in any browser or device).
fn check(ui: Ui, checking: RwSignal<bool>, manual: bool) {
    if checking.get_untracked() || ui.busy.get_untracked() {
        return;
    }
    let Some(token) = ui.token.get_untracked() else {
        return;
    };
    checking.set(true);
    leptos::task::spawn_local(async move {
        let result = client::request(&AccountRequest::Current, Some(&token)).await;
        let _ = checking.try_set(false);
        // Never apply an old session's result to a newly signed-in account.
        if ui.token.get_untracked().as_deref() != Some(&token) {
            return;
        }
        match result {
            Ok(AccountResponse::Account { account }) if account.email_verified => {
                apply(ui, AccountResponse::Account { account }).await;
                ui.status.set("Email verified. Welcome to Thiscord!".into());
            }
            Ok(AccountResponse::Account { account }) => {
                // Another device may have corrected the address.
                if ui
                    .account
                    .with_untracked(|a| a.as_ref().is_some_and(|a| a.email != account.email))
                {
                    ui.account.set(Some(account));
                }
                if manual {
                    ui.status
                        .set("Not verified yet. Open the link in the email we sent.".into());
                }
            }
            Ok(_) => {}
            Err(error) if manual => ui.status.set(error),
            Err(_) => {}
        }
    });
}

#[component]
pub(super) fn Gate(ui: Ui) -> impl IntoView {
    let checking = RwSignal::new(false);
    let changing = RwSignal::new(false);
    let new_email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let resent_at = RwSignal::new(None::<f64>);
    let now = RwSignal::new(js_sys::Date::now());
    // Sign-in confirmations belong to the previous screen.
    if ui
        .status
        .with_untracked(|s| matches!(s.as_str(), "Signed in" | "Account loaded" | "Working…"))
    {
        ui.status.set(String::new());
    }
    let ticks = std::cell::Cell::new(0_u32);
    let timer = set_interval_with_handle(
        move || {
            now.set(js_sys::Date::now());
            ticks.set(ticks.get().wrapping_add(1));
            if ticks.get().is_multiple_of(CHECK_TICKS) && !document().hidden() {
                check(ui, checking, false);
            }
        },
        Duration::from_secs(1),
    )
    .ok();
    let focus = window_event_listener(leptos::ev::focus, move |_| check(ui, checking, false));
    on_cleanup(move || {
        if let Some(timer) = timer {
            timer.clear();
        }
        focus.remove();
    });
    let email = move || ui.account.with(|a| a.as_ref().map(|a| a.email.clone()));
    let cooldown = move || {
        resent_at
            .get()
            .map(|at| ((at + RESEND_COOLDOWN_MS - now.get()) / 1000.0).ceil())
            .filter(|seconds| *seconds > 0.0)
            .map(|seconds| seconds as u32)
    };
    let resend = move |_| {
        resent_at.set(Some(js_sys::Date::now()));
        run(ui, AccountRequest::SendVerification);
    };
    let change = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if ui.busy.get_untracked() {
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        let address = new_email.get_untracked();
        let secret = password.get_untracked();
        password.set(String::new());
        ui.busy.set(true);
        ui.status.set("Working…".into());
        leptos::task::spawn_local(async move {
            // Changing an identifier needs recent reauthentication.
            let result = async {
                client::request(
                    &AccountRequest::Reauthenticate { password: secret },
                    Some(&token),
                )
                .await?;
                client::request(
                    &AccountRequest::ChangeEmail { email: address },
                    Some(&token),
                )
                .await
            }
            .await;
            ui.busy.set(false);
            if ui.token.get_untracked().as_deref() != Some(&token) {
                return;
            }
            match result {
                Ok(AccountResponse::Account { account }) => {
                    let address = account.email.clone();
                    ui.account.set(Some(account));
                    let _ = changing.try_set(false);
                    let _ = new_email.try_set(String::new());
                    let _ = resent_at.try_set(Some(js_sys::Date::now()));
                    ui.status.set(thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "We sent a new link to {}",
                        address
                    ));
                }
                Ok(_) => ui.status.set("Unexpected server response".into()),
                Err(error) => ui.status.set(error),
            }
        });
    };
    view! {
        <main class="auth-page">
            <div class="auth-language"><crate::i18n::LanguageSelector compact=true/></div>
            <section class="auth-card space-y-6" aria-labelledby="verify-title">
                <header class="text-center">
                    <span class="auth-mark"><crate::brand::BrandMark class="h-7 w-7"/></span>
                    <h1 id="verify-title" class="auth-title">{move || crate::i18n::t("Check your email")}</h1>
                    <p class="auth-subtitle">{move || crate::i18n::t("To finish creating your account, open the link we sent to")}</p>
                    <p class="mt-1 break-all font-semibold">{email}</p>
                </header>
                <p class="text-sm text-muted">{move || crate::i18n::t("Thiscord continues automatically once your email is verified, even if you open the link on another device. Links expire after 30 minutes.")}</p>
                <Show when=move || !changing.get() fallback=move || view! {
                    <form class="space-y-4" on:submit=change>
                        <Field label="New email" value=new_email kind="email" autocomplete="email"/>
                        <Field label="Current password" value=password kind="password" autocomplete="current-password"/>
                        <div class="flex gap-3">
                            <button type="submit" class="btn btn-primary flex-1" disabled=move || ui.busy.get()>{move || crate::i18n::t("Send new link")}</button>
                            <button type="button" class="btn btn-secondary flex-1" disabled=move || ui.busy.get()
                                on:click=move |_| { changing.set(false); password.set(String::new()); }>{move || crate::i18n::t("Cancel")}</button>
                        </div>
                    </form>
                }>
                    <div class="space-y-3">
                        <button type="button" class="btn btn-primary btn-block" disabled=move || ui.busy.get() || checking.get()
                            on:click=move |_| check(ui, checking, true)>{move || if checking.get() { crate::i18n::t("Checking…") } else { crate::i18n::t("I've verified my email") }}</button>
                        <button type="button" class="btn btn-secondary btn-block" disabled=move || ui.busy.get() || cooldown().is_some()
                            on:click=resend>{move || match cooldown() {
                                Some(seconds) => thiscord_frontend::localized_format!(crate::i18n::language(), "Resend email in {}s", seconds),
                                None => crate::i18n::t("Resend email").to_owned(),
                            }}</button>
                    </div>
                    <p class="text-center text-sm text-muted">{move || crate::i18n::t("Wrong address?")}" "
                        <button type="button" class="text-link" disabled=move || ui.busy.get()
                            on:click=move |_| { ui.status.set(String::new()); changing.set(true); }>{move || crate::i18n::t("Change email")}</button></p>
                </Show>
                <p class="auth-status" role="status" aria-live="polite">{move || crate::i18n::message(ui.status.get())}</p>
                <p class="text-center text-sm text-muted">{move || crate::i18n::t("Not your account?")}" "
                    <button type="button" class="text-link" disabled=move || ui.busy.get()
                        on:click=move |_| run(ui, AccountRequest::Logout)>{move || crate::i18n::t("Sign out")}</button></p>
            </section>
        </main>
    }
}
