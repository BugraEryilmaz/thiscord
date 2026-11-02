use crate::account_client as client;
use leptos::prelude::*;
use thiscord_shared::account::*;
mod admin;
mod audio;
mod camera;
mod chat;
mod context_menu;
mod diagnostics;
mod dialog;
mod direct_attachments;
mod emoji;
mod friends;
mod icons;
mod invites;
mod login;
mod message_content;
mod moderation;
mod notifications;
mod permissions;
mod profile;
mod safety;
mod screen;
mod screen_player;
mod scripting_bots;
mod search;
mod servers;
mod settings;
mod shortcuts;
mod soundboard;
mod tooltip;
mod updates;
mod voice_bots;
mod window;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Draft {
    Channel(thiscord_shared::ChannelId),
    Conversation(thiscord_shared::ids::ConversationId),
}
/// Binds a composer signal to the per-target draft store: restore on mount and
/// save every change while not `paused`, so switching channels or conversations keeps unsent text.
fn draft_signal(ui: Ui, key: Draft, paused: impl Fn() -> bool + 'static) -> RwSignal<String> {
    let draft = RwSignal::new(
        ui.drafts
            .with_value(|drafts| drafts.get(&key).cloned())
            .unwrap_or_default(),
    );
    Effect::new(move |_| {
        let text = draft.get();
        if paused() {
            return;
        }
        ui.drafts.update_value(|drafts| {
            if text.is_empty() {
                drafts.remove(&key);
            } else {
                drafts.insert(key, text);
            }
        });
    });
    draft
}

#[derive(Clone, Copy)]
struct Ui {
    invites: invites::State,
    bot_target: RwSignal<Option<voice_bots::Target>>,
    notifications: notifications::State,
    friends: friends::State,
    audio_control: crate::audio_control::AudioControl,
    updates: RwSignal<thiscord_shared::update::UpdateStatus>,
    update_error: RwSignal<String>,
    audio: RwSignal<thiscord_shared::audio::AudioSettings>,
    audio_saving: RwSignal<bool>,
    audio_revision: RwSignal<u64>,
    audio_status: RwSignal<Option<thiscord_shared::audio::AudioStatus>>,
    voice: RwSignal<thiscord_shared::voice::VoiceStatus>,
    voice_location: RwSignal<Option<audio::VoiceLocation>>,
    voice_joining: RwSignal<bool>,
    active_stream: RwSignal<Option<screen::StreamTarget>>,
    selected_channel: RwSignal<Option<thiscord_shared::permissions::Channel>>,
    sidebar_preferences: RwSignal<thiscord_frontend::sidebar::Preferences>,
    context_menu: RwSignal<Option<context_menu::Menu>>,
    context_generation: RwSignal<u64>,
    role_target: RwSignal<Option<(thiscord_shared::GuildId, thiscord_shared::AccountId)>>,
    permission_channel: RwSignal<Option<(thiscord_shared::GuildId, thiscord_shared::ChannelId)>>,
    voice_roster: RwSignal<Vec<thiscord_shared::voice::ChannelRoster>>,
    socket_status: RwSignal<chat::ConnectionStatus>,
    chat_epoch: RwSignal<u64>,
    active_chat: RwSignal<Option<chat::Chat>>,
    chat_jump: RwSignal<Option<(thiscord_shared::ChannelId, thiscord_shared::MessageId)>>,
    /// Unsent composer text per channel or conversation, in memory for this session only.
    drafts: StoredValue<std::collections::HashMap<Draft, String>>,
    unread: RwSignal<Vec<thiscord_shared::chat::Unread>>,
    guilds: RwSignal<Vec<thiscord_shared::permissions::Guild>>,
    server: RwSignal<Option<thiscord_shared::permissions::GuildHome>>,
    navigation: RwSignal<thiscord_frontend::navigation::Navigation>,
    server_error: RwSignal<String>,
    server_refresh: RwSignal<u64>,
    can_create_server: RwSignal<bool>,
    instance_access: RwSignal<Option<thiscord_shared::permissions::InstanceAccess>>,
    moderation_target: RwSignal<Option<thiscord_shared::AccountId>>,
    account: RwSignal<Option<Account>>,
    token: RwSignal<Option<String>>,
    status: RwSignal<String>,
    busy: RwSignal<bool>,
    page: RwSignal<&'static str>,
    settings_dirty: RwSignal<bool>,
    settings_pending: RwSignal<Option<&'static str>>,
    sessions: RwSignal<Vec<SessionInfo>>,
    ticket: RwSignal<Option<String>>,
    authorization_url: RwSignal<Option<String>>,
    /// Narrow screens show either navigation or the open conversation; this is
    /// the user's latest choice. See [`mobile_content`] for the effective view.
    mobile_content: RwSignal<bool>,
}
/// On phones, content replaces navigation only while something is open in the
/// current context; otherwise navigation stays reachable.
fn mobile_content(ui: Ui) -> bool {
    ui.mobile_content.get()
        && if ui.navigation.with(|n| n.current().guild.is_some()) {
            ui.server.with(Option::is_some) && ui.selected_channel.with(Option::is_some)
        } else {
            ui.friends.selected.with(Option::is_some)
        }
}
async fn apply(ui: Ui, response: AccountResponse) {
    match response {
        AccountResponse::Session { session } => {
            let persistence = client::persist(Some(&session.token)).await;
            servers::reset_session(ui);
            ui.token.set(Some(session.token));
            ui.account.set(Some(session.account));
            ui.page.set("servers");
            ui.status.set(
                persistence
                    .err()
                    .map(|e| {
                        thiscord_frontend::localized_format!(
                            crate::i18n::language(),
                            "Signed in for this window, but {e}",
                            e = e
                        )
                    })
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
            ui.status.set("Active devices loaded".into());
        }
        AccountResponse::Done { message } => ui.status.set(message),
        _ => {}
    }
}
async fn clear_local(ui: Ui) {
    servers::reset_session(ui);
    ui.drafts.update_value(std::collections::HashMap::clear);
    ui.guilds.set(vec![]);
    ui.voice_location.set(None);
    ui.active_stream.set(None);
    ui.can_create_server.set(false);
    ui.instance_access.set(None);
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
                    ui.status.set(thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "Cleared this window's session. Server logout was not confirmed: {error}",
                        error = crate::i18n::message(error)
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
    } else if client::admin_page() {
        crate::i18n::t("Thiscord admin browser")
    } else {
        crate::i18n::t("Thiscord web")
    }
}
/// Browsers report the on-screen keyboard through the visual viewport. Narrow
/// layouts size the shell to it so the composer stays above the keyboard.
fn track_visual_viewport() {
    use wasm_bindgen::{JsCast, prelude::Closure};
    let Some(viewport) = js_sys::Reflect::get(&window(), &"visualViewport".into())
        .ok()
        .filter(|v| v.is_object())
    else {
        return;
    };
    let update = {
        let viewport = viewport.clone();
        move || {
            if let Some(height) = js_sys::Reflect::get(&viewport, &"height".into())
                .ok()
                .and_then(|v| v.as_f64())
                && let Some(root) = document()
                    .document_element()
                    .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
            {
                // CSSOM, not a `style` attribute: the web CSP forbids inline styles.
                let _ = root
                    .style()
                    .set_property("--viewport-height", &format!("{height}px"));
            }
        }
    };
    update();
    let listener = Closure::<dyn FnMut()>::new(update);
    let target: web_sys::EventTarget = viewport.unchecked_into();
    let _ = target.add_event_listener_with_callback("resize", listener.as_ref().unchecked_ref());
    // The app shell lives for the whole page.
    listener.forget();
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
                // Browsers may still treat this as part of the click. Popup
                // blockers (and mobile Safari) keep the visible link instead.
                client::open_tab(&authorization_url);
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
    view! { <label class="block space-y-2 text-sm font-medium"><span>{move || crate::i18n::t(label)}</span>
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
            <div class="fixed left-3 right-3 top-[calc(env(safe-area-inset-top)+0.75rem)] z-50 flex items-start gap-4 rounded-lg border border-white/10 bg-slate-900 p-4 shadow-lg md:bottom-5 md:left-auto md:right-5 md:top-auto md:max-w-sm">
                <p class="text-sm" role="status" aria-live="polite">{move || crate::i18n::message(ui.status.get())}</p>
                <button class="text-white/60 hover:text-white" aria-label=move || crate::i18n::t("Dismiss notification") on:click=move |_| visible.set(false)>"×"</button>
            </div>
        </Show>
    }
}
#[component]
pub fn App() -> impl IntoView {
    let ui = Ui {
        invites: invites::State::new(),
        bot_target: RwSignal::new(None),
        friends: friends::State::new(),
        notifications: notifications::State::new(),
        audio_control: Default::default(),
        updates: RwSignal::new(Default::default()),
        update_error: RwSignal::new(String::new()),
        audio: RwSignal::new(Default::default()),
        audio_saving: RwSignal::new(false),
        audio_revision: RwSignal::new(0),
        audio_status: RwSignal::new(None),
        voice: RwSignal::new(Default::default()),
        voice_location: RwSignal::new(None),
        voice_joining: RwSignal::new(false),
        active_stream: RwSignal::new(None),
        selected_channel: RwSignal::new(None),
        sidebar_preferences: RwSignal::new(Default::default()),
        context_menu: RwSignal::new(None),
        context_generation: RwSignal::new(0),
        role_target: RwSignal::new(None),
        permission_channel: RwSignal::new(None),
        voice_roster: RwSignal::new(vec![]),
        socket_status: RwSignal::new(chat::ConnectionStatus::Offline),
        chat_epoch: RwSignal::new(0),
        active_chat: RwSignal::new(None),
        chat_jump: RwSignal::new(None),
        drafts: StoredValue::new(Default::default()),
        unread: RwSignal::new(vec![]),
        guilds: RwSignal::new(vec![]),
        server: RwSignal::new(None),
        navigation: RwSignal::new(Default::default()),
        server_error: RwSignal::new(String::new()),
        server_refresh: RwSignal::new(0),
        can_create_server: RwSignal::new(false),
        instance_access: RwSignal::new(None),
        moderation_target: RwSignal::new(None),
        account: RwSignal::new(None),
        token: RwSignal::new(None),
        status: RwSignal::new("Welcome to Thiscord".into()),
        busy: RwSignal::new(false),
        page: RwSignal::new("login"),
        settings_dirty: RwSignal::new(false),
        settings_pending: RwSignal::new(None),
        sessions: RwSignal::new(vec![]),
        ticket: RwSignal::new(None),
        authorization_url: RwSignal::new(None),
        mobile_content: RwSignal::new(false),
    };
    if crate::account_client::admin_page() {
        return view! {
            <tooltip::Host/><context_menu::Host ui=ui/><voice_bots::Host ui=ui/>
            <Show when=move ||ui.account.get().is_some() fallback=move ||view!{<login::Login ui=ui admin=true/>}>
                <admin::Dashboard ui=ui/>
            </Show>
        }.into_any();
    }
    if client::desktop() {
        ui.busy.set(true);
        leptos::task::spawn_local(async move {
            match client::native::<Option<String>>("load_session", serde_json::json!({})).await {
                Ok(Some(token)) => {
                    match client::request(&AccountRequest::Rotate, Some(&token)).await {
                        Ok(response) => apply(ui, response).await,
                        Err(error) => ui.status.set(thiscord_frontend::localized_format!(
                            crate::i18n::language(),
                            "Could not restore session: {error}. Sign in to continue",
                            error = crate::i18n::message(error)
                        )),
                    }
                }
                Ok(None) => {}
                Err(error) => ui.status.set(error),
            }
            ui.busy.set(false);
        });
    }
    // The web client restores its HttpOnly cookie session; avoid flashing the
    // login form while that check runs.
    let restoring = RwSignal::new(client::web_app());
    if client::web_app() {
        track_visual_viewport();
        invites::receive_fragment(ui);
        window_event_listener(leptos::ev::hashchange, move |_| {
            invites::receive_fragment(ui)
        });
        leptos::task::spawn_local(async move {
            match client::restore_cookie_session().await {
                Ok(Some(account)) => {
                    servers::reset_session(ui);
                    ui.token.set(Some(String::new()));
                    ui.account.set(Some(account));
                    ui.page.set("servers");
                    ui.status.set(String::new());
                }
                Ok(None) => {}
                Err(error) => ui.status.set(thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "Could not restore session: {error}. Sign in to continue",
                    error = crate::i18n::message(error)
                )),
            }
            restoring.set(false);
        });
    }
    // Selecting a channel or conversation opens it on phones.
    let opened = Memo::new(move |_| {
        (
            ui.selected_channel.with(|c| c.as_ref().map(|c| c.id)),
            ui.friends.selected.get(),
        )
    });
    Effect::new(move |previous: Option<()>| {
        let (channel, conversation) = opened.get();
        if previous.is_some() && (channel.is_some() || conversation.is_some()) {
            ui.mobile_content.set(true);
        }
    });
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
        <tooltip::Host/><context_menu::Host ui=ui/><voice_bots::Host ui=ui/>
        <invites::Host ui=ui/>
        <updates::Host ui=ui/>
        <notifications::Host ui=ui/>
        <Show when=move ||ui.account.get().is_some() fallback=move ||view!{
            <Show when=move ||restoring.get() fallback=move ||view!{<login::Login ui=ui/>}>
                <main class="flex min-h-dvh flex-col items-center justify-center gap-4 text-white/60" aria-busy="true">
                    <crate::brand::BrandMark class="h-12 w-12 animate-pulse text-brand"/>
                    <p role="status">{move || crate::i18n::t("Loading Thiscord…")}</p>
                </main>
            </Show>
        }>
        <div class="app-shell" class:in-voice=move ||ui.voice.get().channel_id.is_some() class:mobile-content=move ||mobile_content(ui)>
        <servers::ServerRail ui=ui/>
        <audio::AudioHost ui=ui/>
        <chat::ChatHost ui=ui/>
        <friends::Host ui=ui/>
        <main class="app-main">
            <StatusNotice ui=ui/>
            <div class="min-h-0 min-w-0 flex-1"><servers::ServerHome ui=ui/></div>
        </main>
        <settings::Host ui=ui/>
        <audio::VoiceBar ui=ui/>
        </div>
        </Show>
    }
    .into_any()
}
