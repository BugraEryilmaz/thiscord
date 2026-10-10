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
            let _ = dialog.show_modal();
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
        <main class="flex min-h-screen items-center justify-center px-5 py-10">
            <section class="w-full max-w-md space-y-6 rounded-2xl border border-white/10 bg-surface p-8 shadow-2xl" aria-labelledby="login-title">
                <header class="space-y-2 text-center"><p class="text-sm font-semibold uppercase tracking-widest text-brand">"Thiscord"</p>
                    <h1 id="login-title" class="text-3xl font-bold">{if admin { "Instance dashboard" } else { "Welcome back" }}</h1><p class="text-white/60">{if admin { "Sign in with an instance owner or admin account." } else { "Sign in to your account." }}</p>
                </header>
                <form class="space-y-5" on:submit=sign_in>
                    <Field label="Username or email" value=login autocomplete="username"/>
                    <Field label="Password" value=password kind="password" autocomplete="current-password"/>
                    <button type="submit" class="w-full rounded-md bg-brand px-5 py-3 font-semibold text-white disabled:opacity-50" disabled=move ||ui.busy.get()>"Sign in"</button>
                </form>
                <button class="w-full rounded-md border border-white/20 px-4 py-3 disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|google(ui,GooglePurpose::Login)>"Continue with Google"</button>
                <div class="flex flex-wrap justify-between gap-3 text-sm">
                    <button class="text-brand underline-offset-4 hover:underline focus-visible:underline disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|open("register")>"Sign up"</button>
                    <button class="text-brand underline-offset-4 hover:underline focus-visible:underline disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|open("forgot")>"Forgot password?"</button>
                </div>
                <p class="text-sm text-white/70" role="status" aria-live="polite">{move ||ui.status.get()}</p>
                <Show when=move ||ui.ticket.get().is_some()>
                    {move ||ui.authorization_url.get().map(|url|view!{<a class="block text-brand underline" href=url target="_blank" rel="noopener noreferrer">"Continue in Google"</a>})}
                    <button class="text-sm underline" on:click=move |_|super::cancel_google(ui)>"Cancel Google sign-in"</button>
                </Show>
            </section>
            <dialog node_ref=dialog aria-labelledby="account-dialog-title"
                class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-md overflow-y-auto rounded-2xl border border-white/15 bg-surface p-7 text-white shadow-2xl backdrop:bg-black/70 backdrop:backdrop-blur-sm"
                on:close=move |_| {new_password.set(String::new());reset_code.set(String::new());}
                on:click=move |event| { if event.target()==event.current_target() && let Some(dialog)=dialog.get() {
                    let bounds=dialog.get_bounding_client_rect();
                    let x=f64::from(event.client_x());let y=f64::from(event.client_y());
                    if x<bounds.left() || x>bounds.right() || y<bounds.top() || y>bounds.bottom() {dialog.close();}
                }}>
                <header class="mb-5 flex items-start justify-between gap-4">
                    <h2 id="account-dialog-title" class="text-2xl font-semibold">{move ||match mode.get(){"register"=>"Create an account","reset"=>"Reset password",_=>"Forgot password?"}}</h2>
                    <button type="button" aria-label="Close dialog" class="rounded px-2 py-1 text-white/70 hover:bg-white/10" on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"✕"</button>
                </header>
                <p class="mb-5 text-sm text-white/60">{move ||match mode.get(){"register"=>"We’ll email you a link to verify your address.","reset"=>"Enter your reset code and choose a new password.",_=>"Enter your email and we’ll send password reset instructions."}}</p>
                <form class="space-y-4" on:submit=submit_modal>
                    <Show when=move ||mode.get()=="register"><Field label="Username" value=username autocomplete="username"/></Show>
                    <Show when=move ||mode.get()!="reset"><Field label="Email" value=email kind="email" autocomplete="email"/></Show>
                    <Show when=move ||mode.get()=="reset"><Field label="Password reset code" value=reset_code autocomplete="one-time-code"/></Show>
                    <Show when=move ||mode.get()!="forgot"><Field label="Password (at least 12 characters)" value=new_password kind="password" autocomplete="new-password"/></Show>
                    <button type="submit" class="w-full rounded-md bg-brand px-5 py-3 font-semibold disabled:opacity-50" disabled=move ||ui.busy.get()>{move ||match mode.get(){"register"=>"Create account","reset"=>"Reset password",_=>"Send reset instructions"}}</button>
                </form>
                <p class="mt-4 text-sm text-white/70" role="status" aria-live="polite">{move ||ui.status.get()}</p>
                <Show when=move ||mode.get()=="forgot"><button class="mt-4 text-sm text-brand hover:underline" disabled=move ||ui.busy.get() on:click=move |_|mode.set("reset")>"I already have a reset code"</button></Show>
                <Show when=move ||mode.get()=="reset"><button class="mt-4 text-sm text-brand hover:underline" disabled=move ||ui.busy.get() on:click=move |_|mode.set("forgot")>"Request another code"</button></Show>
            </dialog>
        </main>
    }
}
