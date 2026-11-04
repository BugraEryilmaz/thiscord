use super::{Field, Ui, device_name, google, run};
use leptos::prelude::*;
use thiscord_shared::account::{AccountRequest, GooglePurpose};

#[component]
pub(super) fn Login(ui: Ui, #[prop(default = false)] admin: bool) -> impl IntoView {
    let login = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let username = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let new_password = RwSignal::new(String::new());
    let reset_code = RwSignal::new(String::new());
    let mode = RwSignal::new("register");
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let open = move |page| {
        mode.set(page);
        new_password.set(String::new());
        reset_code.set(String::new());
        ui.status.set(String::new());
        if let Some(dialog) = dialog.get() {
            super::dialog::open(&dialog);
        }
    };
    let sign_in = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let command = AccountRequest::Login {
            login: login.get_untracked(),
            password: password.get_untracked(),
            device: device_name().into(),
        };
        password.set(String::new());
        run(ui, command);
    };
    let submit_modal = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let command = match mode.get_untracked() {
            "register" => AccountRequest::Register {
                username: username.get_untracked(),
                email: email.get_untracked(),
                password: new_password.get_untracked(),
                device: device_name().into(),
            },
            "reset" => AccountRequest::ResetPassword {
                code: reset_code.get_untracked(),
                password: new_password.get_untracked(),
            },
            _ => AccountRequest::ForgotPassword {
                email: email.get_untracked(),
            },
        };
        new_password.set(String::new());
        reset_code.set(String::new());
        run(ui, command);
    };
    view! {
        <main class="auth-page">
            <div class="auth-language"><crate::i18n::LanguageSelector compact=true/></div>
            <section class="auth-card space-y-6" aria-labelledby="login-title">
                <header class="text-center">
                    <span class="auth-mark"><crate::brand::BrandMark class="h-7 w-7"/></span>
                    <h1 id="login-title" class="auth-title">{move || if admin { crate::i18n::t("Instance dashboard") } else { crate::i18n::t("Welcome back") }}</h1>
                    <p class="auth-subtitle">{move || if admin { crate::i18n::t("Sign in with an instance owner or admin account.") } else { crate::i18n::t("Sign in to your account.") }}</p>
                </header>
                <form class="space-y-4" on:submit=sign_in>
                    <Field label="Username or email" value=login autocomplete="username"/>
                    <Field label="Password" value=password kind="password" autocomplete="current-password"/>
                    <div class="flex justify-end"><button type="button" class="text-link text-sm" disabled=move ||ui.busy.get() on:click=move |_|open("forgot")>{move || crate::i18n::t("Forgot password?")}</button></div>
                    <button type="submit" class="btn btn-primary btn-block" disabled=move ||ui.busy.get()>{move || crate::i18n::t("Sign in")}</button>
                </form>
                <div class="auth-divider" aria-hidden="true">{move || crate::i18n::t("or")}</div>
                <button class="btn btn-secondary btn-block" disabled=move ||ui.busy.get() on:click=move |_|google(ui,GooglePurpose::Login)><GoogleMark/>{move || crate::i18n::t("Continue with Google")}</button>
                <p class="text-center text-sm text-muted">{move || crate::i18n::t("New to Thiscord?")}" "<button class="text-link" disabled=move ||ui.busy.get() on:click=move |_|open("register")>{move || crate::i18n::t("Create an account")}</button></p>
                <p class="auth-status" role="status" aria-live="polite">{move ||crate::i18n::message(ui.status.get())}</p>
                <Show when=move ||ui.ticket.get().is_some()>
                    <div class="flex flex-wrap items-center justify-center gap-4 text-sm">
                        {move ||ui.authorization_url.get().map(|url|view!{<a class="text-link" href=url target="_blank" rel="noopener noreferrer">{move || crate::i18n::t("Continue in Google")}</a>})}
                        <button class="text-link" on:click=move |_|super::cancel_google(ui)>{move || crate::i18n::t("Cancel Google sign-in")}</button>
                    </div>
                </Show>
            </section>
            <dialog node_ref=dialog aria-labelledby="account-dialog-title"
                class="app-dialog max-w-md"
                on:close=move |_| {new_password.set(String::new());reset_code.set(String::new());}
                on:click=move |event| { if event.target()==event.current_target() && let Some(dialog)=dialog.get() {
                    let bounds=dialog.get_bounding_client_rect();
                    let x=f64::from(event.client_x());let y=f64::from(event.client_y());
                    if x<bounds.left() || x>bounds.right() || y<bounds.top() || y>bounds.bottom() {dialog.close();}
                }}>
                <header class="mb-5 flex items-start justify-between gap-4">
                    <h2 id="account-dialog-title" class="dialog-title">{move ||match mode.get(){"register"=>crate::i18n::t("Create an account"),"reset"=>crate::i18n::t("Reset password"),_=>crate::i18n::t("Forgot password?")}}</h2>
                    <button type="button" aria-label=move || crate::i18n::t("Close dialog") class="icon-button -mr-2 -mt-1" on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}><super::icons::Icon kind=super::icons::Glyph::Close/></button>
                </header>
                <p class="dialog-description mb-5">{move ||match mode.get(){"register"=>crate::i18n::t("We’ll email you a link to verify your address."),"reset"=>crate::i18n::t("Enter your backup reset code and choose a new password."),_=>crate::i18n::t("We’ll email you a link to choose a new password in your browser, plus a backup code.")}}</p>
                <form class="space-y-4" on:submit=submit_modal>
                    <Show when=move ||mode.get()=="register"><Field label="Username" value=username autocomplete="username"/></Show>
                    <Show when=move ||mode.get()!="reset"><Field label="Email" value=email kind="email" autocomplete="email"/></Show>
                    <Show when=move ||mode.get()=="reset"><Field label="Password reset code" value=reset_code autocomplete="one-time-code"/></Show>
                    <Show when=move ||mode.get()!="forgot"><Field label="Password (at least 12 characters)" value=new_password kind="password" autocomplete="new-password"/></Show>
                    <button type="submit" class="btn btn-primary btn-block" disabled=move ||ui.busy.get()>{move ||match mode.get(){"register"=>crate::i18n::t("Create account"),"reset"=>crate::i18n::t("Reset password"),_=>crate::i18n::t("Send reset instructions")}}</button>
                </form>
                <p class="auth-status mt-4" role="status" aria-live="polite">{move ||crate::i18n::message(ui.status.get())}</p>
                <Show when=move ||mode.get()=="forgot"><button class="text-link mt-2 text-sm" disabled=move ||ui.busy.get() on:click=move |_|mode.set("reset")>{move || crate::i18n::t("I already have a reset code")}</button></Show>
                <Show when=move ||mode.get()=="reset"><button class="text-link mt-2 text-sm" disabled=move ||ui.busy.get() on:click=move |_|mode.set("forgot")>{move || crate::i18n::t("Request another code")}</button></Show>
            </dialog>
        </main>
    }
}

/// Google's "G" mark for the Google sign-in button, per Google's branding guidance.
#[component]
fn GoogleMark() -> impl IntoView {
    view! {
        <svg class="size-[18px] shrink-0" viewBox="0 0 48 48" aria-hidden="true">
            <path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z"/>
            <path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z"/>
            <path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z"/>
            <path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z"/>
        </svg>
    }
}
