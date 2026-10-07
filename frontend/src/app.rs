use crate::account_client as client;
use leptos::prelude::*;
use thiscord_shared::account::*;
mod audio;
mod chat;
mod login;
mod moderation;
mod permissions;
mod screen;
mod screen_player;
mod servers;
mod updates;

#[derive(Clone, Copy)]
struct Ui {
    updates: RwSignal<thiscord_shared::update::UpdateStatus>,
    update_error: RwSignal<String>,
    audio: RwSignal<thiscord_shared::audio::AudioSettings>,
    audio_saving: RwSignal<bool>,
    audio_revision: RwSignal<u64>,
    audio_status: RwSignal<Option<thiscord_shared::audio::AudioStatus>>,
    voice: RwSignal<thiscord_shared::voice::VoiceStatus>,
    chat_epoch: RwSignal<u64>,
    active_chat: RwSignal<Option<chat::Chat>>,
    unread: RwSignal<Vec<thiscord_shared::chat::Unread>>,
    guilds: RwSignal<Vec<thiscord_shared::permissions::Guild>>,
    server: RwSignal<Option<thiscord_shared::permissions::GuildHome>>,
    can_create_server: RwSignal<bool>,
    moderation_target: RwSignal<Option<thiscord_shared::AccountId>>,
    account: RwSignal<Option<Account>>,
    token: RwSignal<Option<String>>,
    status: RwSignal<String>,
    busy: RwSignal<bool>,
    page: RwSignal<&'static str>,
    sessions: RwSignal<Vec<SessionInfo>>,
    ticket: RwSignal<Option<String>>,
    authorization_url: RwSignal<Option<String>>,
}
async fn apply(ui: Ui, response: AccountResponse) {
    match response {
        AccountResponse::Session { session } => {
            let persistence = client::persist(Some(&session.token)).await;
            ui.token.set(Some(session.token));
            ui.account.set(Some(session.account));
            ui.page.set("servers");
            ui.status.set(
                persistence
                    .err()
                    .map(|e| format!("Signed in for this window, but {e}"))
                    .unwrap_or_else(|| "Signed in".into()),
            );
        }
        AccountResponse::Account { account } => {
            ui.account.set(Some(account));
            servers::refresh(ui).await;
            ui.status.set("Account loaded".into());
        }
        AccountResponse::Sessions { sessions } => {
            ui.sessions.set(sessions);
            ui.page.set("devices");
            ui.status.set("Active devices loaded".into());
        }
        AccountResponse::Done { message } => ui.status.set(message),
        _ => {}
    }
}
async fn clear_local(ui: Ui) {
    ui.guilds.set(vec![]);
    ui.server.set(None);
    ui.can_create_server.set(false);
    ui.token.set(None);
    ui.account.set(None);
    ui.sessions.set(vec![]);
    ui.page.set("login");
    if let Err(error) = client::persist(None).await {
        ui.status.set(error);
    }
}
fn run(ui: Ui, command: AccountRequest) {
    if ui.busy.get_untracked() {
        return;
    }
    ui.busy.set(true);
    ui.status.set("Working…".into());
    leptos::task::spawn_local(async move {
        let clears = matches!(
            command,
            AccountRequest::Logout
                | AccountRequest::LogoutAll
                | AccountRequest::DeleteAccount { .. }
                | AccountRequest::ChangePassword { .. }
                | AccountRequest::UnlinkIdentity { .. }
        ) || matches!(&command, AccountRequest::RevokeSession { session_id } if ui.sessions.get_untracked().iter().any(|s|s.id==*session_id && s.current));
        let token = ui.token.get_untracked();
        match client::request(&command, token.as_deref()).await {
            Ok(response) => {
                apply(ui, response).await;
                if clears {
                    clear_local(ui).await;
                } else if matches!(command, AccountRequest::RevokeSession { .. }) {
                    if let Ok(response) =
                        client::request(&AccountRequest::Sessions, token.as_deref()).await
                    {
                        apply(ui, response).await;
                    }
                } else if matches!(command, AccountRequest::VerifyEmail { .. })
                    && token.is_some()
                    && let Ok(AccountResponse::Account { account }) =
                        client::request(&AccountRequest::Current, token.as_deref()).await
                {
                    ui.account.set(Some(account));
                }
            }
            Err(error) => {
                ui.status.set(error.clone());
                if matches!(command, AccountRequest::Logout) {
                    clear_local(ui).await;
                    ui.status.set(format!(
                        "Cleared this window's session. Server logout was not confirmed: {error}"
                    ));
                }
            }
        }
        ui.busy.set(false);
    });
}
fn device_name() -> &'static str {
    if client::desktop() {
        "Thiscord desktop"
    } else {
        "Browser preview"
    }
}
fn cancel_google(ui: Ui) {
    let ticket = ui.ticket.get_untracked();
    ui.ticket.set(None);
    ui.authorization_url.set(None);
    ui.status.set("Google sign-in cancelled".into());
    leptos::task::spawn_local(async move {
        if let Some(ticket) = ticket {
            let _ = client::request(&AccountRequest::GoogleCancel { ticket }, None).await;
        }
        if client::desktop() {
            let _ = client::native::<()>("cancel_google", serde_json::json!({})).await;
        }
    });
}
fn google(ui: Ui, purpose: GooglePurpose) {
    if ui.busy.get_untracked() {
        return;
    }
    ui.busy.set(true);
    ui.status.set("Preparing Google sign-in…".into());
    leptos::task::spawn_local(async move {
        let result = async {
            let callback = if client::desktop() {
                Some(client::native::<String>("prepare_google", serde_json::json!({})).await?)
            } else {
                None
            };
            let token = ui.token.get_untracked();
            let AccountResponse::GoogleStarted {
                authorization_url,
                ticket,
            } = client::request(
                &AccountRequest::GoogleStart {
                    purpose,
                    callback,
                    device: device_name().into(),
                },
                token.as_deref(),
            )
            .await?
            else {
                return Err("Unexpected Google response".to_string());
            };
            ui.ticket.set(Some(ticket.clone()));
            if client::desktop() {
                client::native::<()>(
                    "open_google",
                    serde_json::json!({"authorizationUrl":authorization_url}),
                )
                .await?;
            } else {
                ui.authorization_url.set(Some(authorization_url));
            }
            ui.status
                .set("Finish signing in with Google in your browser".into());
            for _ in 0..150 {
                gloo_timers::future::TimeoutFuture::new(2000).await;
                if ui.ticket.get_untracked().as_deref() != Some(&ticket) {
                    return Ok(());
                }
                let response = client::request(
                    &AccountRequest::GoogleComplete {
                        ticket: ticket.clone(),
                    },
                    None,
                )
                .await;
                // Cancellation can arrive while redemption is in flight. Never install
                // a late session; revoke it if the backend already issued one.
                if ui.ticket.get_untracked().as_deref() != Some(&ticket) {
                    if let Ok(AccountResponse::Session { session }) = response {
                        let _ =
                            client::request(&AccountRequest::Logout, Some(&session.token)).await;
                    }
                    return Ok(());
                }
                match response? {
                    AccountResponse::Pending => {}
                    response => {
                        ui.ticket.set(None);
                        apply(ui, response).await;
                        if purpose != GooglePurpose::Login
                            && let Ok(AccountResponse::Account { account }) =
                                client::request(&AccountRequest::Current, token.as_deref()).await
                        {
                            ui.account.set(Some(account));
                        }
                        return Ok(());
                    }
                }
            }
            Err("Google sign-in timed out. Try again".to_string())
        }
        .await;
        if let Some(ticket) = ui.ticket.get_untracked() {
            let _ = client::request(&AccountRequest::GoogleCancel { ticket }, None).await;
        }
        if client::desktop() {
            let _ = client::native::<()>("cancel_google", serde_json::json!({})).await;
        }
        ui.ticket.set(None);
        ui.authorization_url.set(None);
        ui.busy.set(false);
        if let Err(error) = result {
            ui.status.set(error);
        }
    });
}
#[component]
fn Field(
    label: &'static str,
    value: RwSignal<String>,
    #[prop(default = "text")] kind: &'static str,
    #[prop(default = "off")] autocomplete: &'static str,
) -> impl IntoView {
    view! { <label class="block space-y-2 text-sm font-medium"><span>{label}</span>
        <input class="w-full rounded-md border border-white/20 bg-black/20 px-3 py-2 text-white focus:border-brand focus:outline-none"
            type=kind autocomplete=autocomplete prop:value=move ||value.get() on:input=move |event|value.set(event_target_value(&event)) />
    </label> }
}
#[component]
fn StatusNotice(ui: Ui) -> impl IntoView {
    let visible = RwSignal::new(false);
    let generation = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let message = ui.status.get();
        let current = generation.get_untracked().wrapping_add(1);
        generation.set(current);
        visible.set(!message.is_empty());
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(5000).await;
            if generation.try_get_untracked() == Some(current) {
                visible.set(false);
            }
        });
    });
    view! {
        <Show when=move || visible.get()>
            <div class="fixed bottom-5 right-5 z-50 flex max-w-sm items-start gap-4 rounded-lg border border-white/10 bg-slate-900 p-4 shadow-lg">
                <p class="text-sm" role="status" aria-live="polite">{move || ui.status.get()}</p>
                <button class="text-white/60 hover:text-white" aria-label="Dismiss notification" on:click=move |_| visible.set(false)>"×"</button>
            </div>
        </Show>
    }
}
#[component]
pub fn App() -> impl IntoView {
    let ui = Ui {
        updates: RwSignal::new(Default::default()),
        update_error: RwSignal::new(String::new()),
        audio: RwSignal::new(Default::default()),
        audio_saving: RwSignal::new(false),
        audio_revision: RwSignal::new(0),
        audio_status: RwSignal::new(None),
        voice: RwSignal::new(Default::default()),
        chat_epoch: RwSignal::new(0),
        active_chat: RwSignal::new(None),
        unread: RwSignal::new(vec![]),
        guilds: RwSignal::new(vec![]),
        server: RwSignal::new(None),
        can_create_server: RwSignal::new(false),
        moderation_target: RwSignal::new(None),
        account: RwSignal::new(None),
        token: RwSignal::new(None),
        status: RwSignal::new("Welcome to Thiscord".into()),
        busy: RwSignal::new(false),
        page: RwSignal::new("login"),
        sessions: RwSignal::new(vec![]),
        ticket: RwSignal::new(None),
        authorization_url: RwSignal::new(None),
    };
    let password = RwSignal::new(String::new());
    let display_name = RwSignal::new(String::new());
    let bio = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    Effect::new(move |_| {
        if let Some(a) = ui.account.get() {
            display_name.set(a.display_name);
            bio.set(a.bio);
        }
    });
    if client::desktop() {
        ui.busy.set(true);
        leptos::task::spawn_local(async move {
            match client::native::<Option<String>>("load_session", serde_json::json!({})).await {
                Ok(Some(token)) => {
                    match client::request(&AccountRequest::Rotate, Some(&token)).await {
                        Ok(response) => apply(ui, response).await,
                        Err(error) => ui.status.set(format!(
                            "Could not restore session: {error}. Sign in to continue"
                        )),
                    }
                }
                Ok(None) => {}
                Err(error) => ui.status.set(error),
            }
            ui.busy.set(false);
        });
    }
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let command = match ui.page.get_untracked() {
            "reauthenticate" => AccountRequest::Reauthenticate {
                password: password.get_untracked(),
            },
            "password" => AccountRequest::ChangePassword {
                password: password.get_untracked(),
            },
            "delete" => AccountRequest::DeleteAccount {
                confirmation: confirmation.get_untracked(),
            },
            _ => AccountRequest::UpdateProfile {
                display_name: display_name.get_untracked(),
                bio: bio.get_untracked(),
            },
        };
        password.set(String::new());
        run(ui, command);
    };
    let cancel = move |_| cancel_google(ui);
    window_event_listener(leptos::ev::focus, move |_| {
        if !ui.busy.get_untracked()
            && ui
                .account
                .get_untracked()
                .is_some_and(|a| !a.email_verified)
        {
            run(ui, AccountRequest::Current);
        }
    });
    view! {
        <updates::Host ui=ui/>
        <Show when=move ||ui.account.get().is_some() fallback=move ||view!{<login::Login ui=ui/>}>
        <servers::ServerRail ui=ui/>
        <audio::AudioHost ui=ui/>
        <chat::ChatHost ui=ui/>
        <main class="ml-20 flex h-dvh min-w-0 flex-col gap-4 overflow-hidden p-4 md:p-6">
            <header class="flex shrink-0 flex-wrap items-center justify-between gap-4">
                <div><h1 class="text-4xl font-bold">"Thiscord"</h1><p class="mt-2 text-white/60">"Your place to chat and hang out."</p></div>
                <Show when=move ||ui.account.get().is_some()><button class="rounded-md bg-white/10 px-4 py-2" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::Logout)>"Sign out"</button></Show>
            </header>
            <StatusNotice ui=ui/>
            <Show when=move ||ui.ticket.get().is_some()><div class="flex flex-wrap gap-4 rounded-lg bg-white/5 p-4">
                {move ||ui.authorization_url.get().map(|url|view!{<a class="text-brand underline" href=url target="_blank" rel="noopener noreferrer">"Continue in Google"</a>})}
                <button class="underline" on:click=cancel>"Cancel Google sign-in"</button>
            </div></Show>
            <Show when=move ||ui.page.get()=="servers"><div class="min-h-0 min-w-0 flex-1"><servers::ServerHome ui=ui/></div></Show>
            <Show when=move ||ui.page.get()!="servers">
            <div class="grid min-h-0 flex-1 gap-6 overflow-y-auto md:grid-cols-[220px_1fr]">
                <nav class="flex flex-col gap-2" aria-label="Account navigation">
                    {move || {
                        let tabs = vec![("profile","Profile"),("audio","Audio & voice"),("updates","App updates"),("permissions","Guilds & roles"),("reauthenticate","Reauthenticate"),("password","Set / change password"),("devices","Devices & identities"),("delete","Delete account")];
                        tabs.into_iter().map(move |(page,label)|view!{
                            <button class="rounded-md px-4 py-3 text-left hover:bg-white/10 disabled:opacity-50" class:bg-brand=move ||ui.page.get()==page disabled=move ||ui.busy.get()
                                on:click=move |_| {password.set(String::new());ui.page.set(page);if page=="devices" {run(ui,AccountRequest::Sessions);}}>{label}</button>
                        }).collect_view()
                    }}
                </nav>
                <section class="space-y-5 rounded-xl border border-white/10 bg-white/5 p-6">
                    <Show when=move ||ui.page.get()=="moderation"><moderation::ModerationPanel ui=ui/></Show>
                    <Show when=move ||ui.page.get()=="permissions"><permissions::PermissionEditor ui=ui/></Show>
                    <Show when=move ||ui.page.get()=="audio"><audio::AudioSettingsPanel ui=ui/></Show>
                    <Show when=move ||ui.page.get()=="updates"><updates::Panel ui=ui/></Show>
                    <Show when=move ||ui.page.get()=="devices">
                        <h2 class="text-xl font-semibold">"Devices & identities"</h2>
                        <ul class="space-y-3">{move ||ui.sessions.get().into_iter().map(|s| {let id=s.id; view!{
                            <li class="flex flex-wrap items-center justify-between gap-3 rounded-md bg-black/20 p-3">
                                <div><p>{s.device}{if s.current {" (this device)"} else {""}}</p><p class="text-xs text-white/60">{format!("Last active: {} · Expires: {}",s.last_seen_at,s.expires_at)}</p></div>
                                <button class="underline" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::RevokeSession {session_id:id})>"Revoke"</button>
                            </li>}}).collect_view()}</ul>
                        <button class="rounded-md bg-white/10 px-4 py-2" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::LogoutAll)>"Sign out all devices"</button>
                        <p class="text-sm text-white/60">"Reauthenticate before linking or removing an identity. Keep at least one login method."</p>
                        {move ||ui.account.get().map(|a|a.identities.into_iter().map(|provider|view!{
                            <div class="flex justify-between"><span>{format!("{provider:?}")}</span><button class="underline" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::UnlinkIdentity {provider})>"Remove identity"</button></div>
                        }).collect_view())}
                        <button class="rounded-md bg-brand px-4 py-2" disabled=move ||ui.busy.get() on:click=move |_|google(ui,GooglePurpose::Link)>"Link Google account"</button>
                    </Show>
                    <Show when=move ||!matches!(ui.page.get(),"devices"|"permissions"|"moderation"|"audio"|"updates")>
                        <h2 class="text-xl font-semibold">{move ||match ui.page.get(){"register"=>"Create your account","login"=>"Welcome back","forgot"=>"Request a password reset","reset"=>"Reset your password","verify"=>"Verify your email","reauthenticate"=>"Confirm it’s you","password"=>"Set or change password","delete"=>"Permanently delete account",_=>"Your profile"}}</h2>
                        <form class="space-y-4" on:submit=submit>
                            <Show when=move ||ui.page.get()=="reauthenticate"><Field label="Current password" value=password kind="password" autocomplete="current-password"/></Show>
                            <Show when=move ||ui.page.get()=="password"><Field label="New password (at least 12 characters)" value=password kind="password" autocomplete="new-password"/></Show>
                            <Show when=move ||ui.page.get()=="profile">
                                <p class="text-sm text-white/60">{move ||ui.account.get().map(|a|format!("@{} · {} · {}",a.username,a.email,if a.email_verified {"Email verified"} else {"Email not verified"}))}</p>
                                <Field label="Display name" value=display_name/><Field label="Bio (up to 500 characters)" value=bio/>
                                <button type="button" class="underline" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::Current)>"Refresh account"</button>
                                <Show when=move ||ui.account.get().is_some_and(|a|!a.email_verified)>
                                    <p class="text-sm text-white/70">"Check your email and click the verification link. Your status updates when you return to Thiscord."</p>
                                    <button type="button" class="text-brand underline" disabled=move ||ui.busy.get() on:click=move |_|run(ui,AccountRequest::SendVerification)>"Resend verification link"</button>
                                </Show>
                            </Show>
                            <Show when=move ||matches!(ui.page.get(),"password"|"delete")><p class="text-sm text-white/60">"Reauthenticate first. This action signs you out on every device."</p></Show>
                            <Show when=move ||ui.page.get()=="delete"><p>"This permanently deletes your profile, credentials and sessions."</p><Field label="Type your username to confirm" value=confirmation/></Show>
                            <button class="rounded-md bg-brand px-5 py-3 font-semibold disabled:opacity-50" type="submit" disabled=move ||ui.busy.get()>{move ||if ui.page.get()=="delete" {"Permanently delete"} else {"Continue"}}</button>
                        </form>
                        <Show when=move ||ui.page.get()=="reauthenticate">
                            <button class="w-full rounded-md border border-white/20 px-4 py-3 disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|google(ui,if ui.page.get_untracked()=="reauthenticate" {GooglePurpose::Reauthenticate} else {GooglePurpose::Login})>"Continue with Google"</button>
                        </Show>
                    </Show>
                </section>
            </div>
            </Show>
            <audio::VoiceBar ui=ui/>
        </main>
        </Show>
    }
}
