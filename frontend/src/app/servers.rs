use super::icons::{Glyph, Icon};
use super::{Field, Ui};
use leptos::prelude::*;
use thiscord_frontend::navigation::Selection;
use thiscord_shared::{GuildId, permissions::*};

pub(super) fn current(ui: Ui, selection: Selection) -> bool {
    ui.navigation
        .try_get_untracked()
        .is_some_and(|navigation| navigation.accepts(selection))
}

pub(super) fn home(ui: Ui) {
    ui.navigation.update(|navigation| {
        navigation.begin(None);
    });
    ui.server_error.set(String::new());
    ui.server.set(None);
    ui.selected_channel.set(None);
    ui.voice_roster.set(vec![]);
    ui.page.set("servers");
}

pub(super) fn reset_session(ui: Ui) {
    home(ui);
    ui.server_refresh
        .update(|generation| *generation = generation.wrapping_add(1));
}

fn update_home(ui: Ui, selection: Selection, home: GuildHome) {
    if !current(ui, selection) || selection.guild != Some(home.guild.id) {
        return;
    }
    if ui
        .selected_channel
        .get_untracked()
        .is_some_and(|channel| !home.channels.iter().any(|allowed| allowed.id == channel.id))
    {
        ui.selected_channel.set(None);
    }
    let unchanged = ui.server.with_untracked(|current| {
        current.as_ref().is_some_and(|old| {
            old.guild.id == home.guild.id
                && old.guild.revision == home.guild.revision
                && old.guild.name == home.guild.name
                && old.guild.owner == home.guild.owner
                && old.can_manage_roles == home.can_manage_roles
                && old.can_manage_channels == home.can_manage_channels
                && old.can_moderate == home.can_moderate
                && old
                    .channels
                    .iter()
                    .map(|c| (c.id, &c.name, c.kind))
                    .eq(home.channels.iter().map(|c| (c.id, &c.name, c.kind)))
        })
    });
    // A profile refresh must not recreate the open conversation and its draft.
    if !unchanged {
        ui.server.set(Some(home));
    }
}

pub(super) async fn request(
    ui: Ui,
    command: PermissionRequest,
) -> Result<PermissionResponse, String> {
    let token = ui.token.get_untracked();
    let response =
        crate::account_client::api_request(PERMISSIONS_PATH, &command, token.as_deref()).await;
    if token != ui.token.get_untracked() {
        return Err("Session changed; refresh your servers".into());
    }
    response
}

// Bound reads only: a timeout on a mutation would leave its commit outcome uncertain.
async fn read(ui: Ui, command: PermissionRequest) -> Result<PermissionResponse, String> {
    match futures_util::future::select(
        Box::pin(request(ui, command)),
        Box::pin(gloo_timers::future::TimeoutFuture::new(10_000)),
    )
    .await
    {
        futures_util::future::Either::Left((result, _)) => result,
        _ => Err("Server request timed out. Please retry.".into()),
    }
}

pub(super) async fn refresh(ui: Ui) {
    let selection = ui.navigation.get_untracked().current();
    let token = ui.token.get_untracked();
    ui.server_refresh
        .update(|generation| *generation = generation.wrapping_add(1));
    let generation = ui.server_refresh.get_untracked();
    let active =
        || ui.token.get_untracked() == token && ui.server_refresh.get_untracked() == generation;
    let result = read(ui, PermissionRequest::ListGuilds {}).await;
    if !active() {
        return;
    }
    match result {
        Ok(PermissionResponse::Guilds { guilds }) => {
            ui.guilds.set(guilds.clone());
            // Selection owns its ViewGuild response while loading. A list is not
            // an access grant and must not restore the previous guild's content.
            if current(ui, selection)
                && !ui.navigation.get_untracked().pending
                && let Some(id) = selection.guild
            {
                let result = if guilds.iter().any(|guild| guild.id == id) {
                    read(ui, PermissionRequest::ViewGuild { guild_id: id }).await
                } else {
                    Err("This server is no longer available.".into())
                };
                if !active() || !current(ui, selection) {
                    return;
                }
                match result {
                    Ok(PermissionResponse::Home { home }) if home.guild.id == id => {
                        ui.server_error.set(String::new());
                        update_home(ui, selection, home);
                    }
                    result => {
                        ui.server.set(None);
                        ui.selected_channel.set(None);
                        ui.voice_roster.set(vec![]);
                        ui.server_error.set(
                            result
                                .err()
                                .unwrap_or_else(|| "Unexpected server response".into()),
                        );
                    }
                }
            }
        }
        Err(error) => ui.status.set(error),
        _ => {}
    }
    let result = read(ui, PermissionRequest::Instance {}).await;
    if !active() {
        return;
    }
    let access = match result {
        Ok(PermissionResponse::Instance { access }) => Some(access),
        _ => None,
    };
    ui.can_create_server
        .set(ui.account.get_untracked().is_some_and(|a| a.email_verified));
    ui.instance_access.set(access);
}

/// Returns false when superseded. Callers must not run navigation follow-up
/// actions (open settings, select a channel, join a stream) after a stale result.
pub(super) fn select(ui: Ui, id: GuildId) -> impl Future<Output = Result<bool, String>> {
    ui.navigation.update(|navigation| {
        navigation.begin(Some(id));
    });
    let selection = ui.navigation.get_untracked().current();
    let token = ui.token.get_untracked();
    ui.server_error.set(String::new());
    // Context-menu settings also reauthorize the current guild. Keep its live
    // conversation/draft mounted while that request is pending; switching to a
    // different guild must still hide the old content immediately.
    if ui
        .server
        .with_untracked(|home| home.as_ref().is_none_or(|home| home.guild.id != id))
    {
        ui.server.set(None);
        ui.selected_channel.set(None);
        ui.voice_roster.set(vec![]);
    }
    ui.page.set("servers");
    async move {
        if !current(ui, selection) || ui.token.get_untracked() != token {
            return Ok(false);
        }
        let result = read(ui, PermissionRequest::ViewGuild { guild_id: id }).await;
        if !current(ui, selection) || ui.token.get_untracked() != token {
            return Ok(false);
        }
        ui.navigation.update(|navigation| {
            navigation.finish(selection);
        });
        match result {
            Ok(PermissionResponse::Home { home }) if home.guild.id == id => {
                update_home(ui, selection, home);
                Ok(true)
            }
            result => {
                let error = result
                    .err()
                    .unwrap_or_else(|| "Unexpected server response".into());
                ui.server.set(None);
                ui.selected_channel.set(None);
                ui.voice_roster.set(vec![]);
                ui.server_error.set(error.clone());
                Err(error)
            }
        }
    }
}

fn navigate(ui: Ui, id: GuildId) {
    // Keep the open conversation/draft and don't restart a pending selection.
    // A failed selection has no loaded view and remains retryable.
    let loaded = ui
        .server
        .with_untracked(|home| home.as_ref().map(|home| home.guild.id));
    if !ui.navigation.get_untracked().needs_navigation(id, loaded) {
        return;
    }
    let selection = select(ui, id);
    leptos::task::spawn_local(async move {
        if let Err(error) = selection.await {
            ui.status.set(error);
        }
    });
}

pub(super) fn open_voice(ui: Ui) {
    let Some(location) = ui.voice_location.get_untracked() else {
        return;
    };
    if location.guild_id.as_uuid() == location.channel_id.as_uuid() {
        if let Some(call) = ui
            .friends
            .snapshot
            .get_untracked()
            .calls
            .into_iter()
            .find(|c| c.voice_channel() == location.channel_id)
        {
            ui.friends.selected.set(Some(call.conversation_id));
            home(ui);
        }
        return;
    }
    leptos::task::spawn_local(async move {
        match select(ui, location.guild_id).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                ui.status.set(error);
                return;
            }
        }
        if ui.voice.get_untracked().channel_id == Some(location.channel_id) {
            ui.selected_channel.set(
                ui.server
                    .get_untracked()
                    .and_then(|s| s.channels.into_iter().find(|c| c.id == location.channel_id)),
            );
        }
    });
}

#[component]
fn GuildButton(ui: Ui, guild: Guild) -> impl IntoView {
    let id = guild.id;
    let initials = guild
        .name
        .split_whitespace()
        .take(2)
        .filter_map(|s| s.chars().next())
        .collect::<String>()
        .to_uppercase();
    view! {
        <button class="guild-button" data-context-server=id.to_string() aria-haspopup="menu" class:selected=move ||ui.navigation.get().current().guild==Some(id) aria-busy=move ||(ui.navigation.get().current().guild==Some(id) && ui.navigation.get().pending).to_string() aria-label=guild.name.clone() data-tip=guild.name
            on:click=move |_|navigate(ui,id)>{initials}</button>
    }
}

#[component]
fn VoiceRoster(ui: Ui, guild: GuildId, channel: thiscord_shared::ChannelId) -> impl IntoView {
    let speaking = Memo::new(move |_| {
        thiscord_frontend::voice_overlay::speaking_in_channel(
            &ui.voice.get(),
            ui.audio_status.get().as_ref(),
            channel,
        )
    });
    let members = Memo::new(move |_| {
        ui.voice_roster
            .get()
            .into_iter()
            .find(|c| c.channel_id == channel)
            .map(|c| c.members)
            .unwrap_or_default()
    });
    view! {<ul class="ml-5 space-y-1 border-l border-white/10 pl-3" aria-label=move || crate::i18n::t("In this voice channel")>
        <For each=move ||members.get() key=|m|m.account_id children=move |initial|{
            let id=initial.account_id;
            let member=Signal::derive(move ||members.get().into_iter().find(|m|m.account_id==id).unwrap_or_else(||initial.clone()));
            view!{<li tabindex="0" aria-haspopup="menu" data-context-server=guild.to_string() data-context-channel=channel.to_string() data-context-member=id.to_string() class="voice-member sidebar-member" class:voice-speaking=move ||speaking.get().contains(&id) data-tip=move ||format!("{} (@{})",member.get().display_name(),member.get().username)>
                <button class="voice-person" class:has-stream=move ||member.get().sharing_screen || member.get().camera_active aria-label=move ||if member.get().sharing_screen || member.get().camera_active{thiscord_frontend::localized_format!(crate::i18n::language(), "Watch {}’s stream",member.get().display_name())}else{member.get().display_name().to_owned()} data-tip=move ||if member.get().sharing_screen || member.get().camera_active{crate::i18n::t("Watch stream")}else{crate::i18n::t("Right-click for options")} on:click=move |_|if member.get_untracked().sharing_screen || member.get_untracked().camera_active{super::screen::open_stream(ui,guild,channel,member.get_untracked());}>
                <span class="voice-avatar"><crate::avatar::Avatar class="size-6 text-xs" name=Signal::derive(move ||member.get().display_name().to_owned()) avatar_id=Signal::derive(move ||member.get().avatar_id)/></span>
                <span class="min-w-0 flex-1 truncate">{move ||member.get().display_name().to_owned()}</span>
                </button><Show when=move ||speaking.get().contains(&id)><span class="sr-only">{move || crate::i18n::t("Speaking")}</span></Show>
                <Show when=move ||member.get().deafened><span aria-label=move || crate::i18n::t("Deafened")><Icon kind=Glyph::Deafen/></span></Show>
                <Show when=move ||!member.get().deafened && (member.get().muted || !member.get().can_speak)><span aria-label=move || crate::i18n::t("Muted")><Icon kind=Glyph::MicOff/></span></Show>
                <Show when=move ||member.get().sharing_screen || member.get().camera_active><button class="stream-live-badge" aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Watch {}’s stream",member.get().display_name()) data-tip=move || crate::i18n::t("Watch stream") on:click=move |_|super::screen::open_stream(ui,guild,channel,member.get_untracked())>{move || crate::i18n::t("LIVE")}</button></Show>
            </li>}
        }/>
    </ul>}
}

#[component]
pub(super) fn ServerRail(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let creating = RwSignal::new(true);
    let name = RwSignal::new(String::new());
    let server_id = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let quota_reached = move || {
        ui.instance_access.get().is_some_and(|a| {
            a.policies
                .iter()
                .find(|p| Some(p.id) == a.policy_id)
                .is_some_and(|p| a.owned_guilds >= p.limits.owned_guilds)
        })
    };
    leptos::task::spawn_local(async move {
        refresh(ui).await;
    });
    let open = move |create| {
        creating.set(create);
        name.set(String::new());
        server_id.set(String::new());
        password.set(String::new());
        error.set(String::new());
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    };
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if ui.busy.get_untracked() {
            return;
        }
        if creating.get_untracked() && quota_reached() {
            return;
        }
        let supplied = password.get_untracked();
        let supplied = (!supplied.is_empty()).then_some(supplied);
        let command = if creating.get_untracked() {
            PermissionRequest::CreateGuild {
                name: name.get_untracked(),
                password: supplied,
            }
        } else {
            let Ok(guild_id) = server_id.get_untracked().trim().parse() else {
                error.set("Enter a valid server ID".into());
                return;
            };
            PermissionRequest::JoinGuild {
                guild_id,
                password: supplied,
            }
        };
        password.set(String::new());
        error.set(String::new());
        ui.busy.set(true);
        leptos::task::spawn_local(async move {
            let selection = ui.navigation.get_untracked().current();
            let token = ui.token.get_untracked();
            let result = request(ui, command).await;
            if !current(ui, selection) || ui.token.get_untracked() != token {
                ui.busy.set(false);
                return;
            }
            match result {
                Ok(response) => {
                    let id = match response {
                        PermissionResponse::State { state } => Some(state.guild.id),
                        PermissionResponse::Joined { guild } => Some(guild.id),
                        _ => None,
                    };
                    refresh(ui).await;
                    if current(ui, selection)
                        && ui.token.get_untracked() == token
                        && let Some(id) = id
                        && let Err(message) = select(ui, id).await
                    {
                        ui.status.set(message);
                    }
                    if let Some(dialog) = dialog.get() {
                        dialog.close();
                    }
                }
                Err(message) => error.set(if creating.get_untracked() {
                    message
                } else {
                    thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "{message}. Check the server ID and password and verify your email.",
                        message = crate::i18n::message(message)
                    )
                }),
            }
            ui.busy.set(false);
        });
    };
    view! {
        <aside class="server-rail" aria-label=move || crate::i18n::t("Servers")>
            <button class="relative mb-4 h-12 w-12 shrink-0 rounded-2xl bg-brand text-xl font-bold" data-tip=move || crate::i18n::t("Thiscord home") aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Thiscord home, {} incoming requests, {} unread messages",ui.friends.pending_count(),super::friends::count_text(ui.friends.unread_count())) on:click=move |_|{ui.friends.selected.set(None);home(ui);}>"T"
                <span class="absolute -bottom-1 -right-1"><super::friends::CountBubble count=Signal::derive(move ||ui.friends.pending_count()+ui.friends.unread_count()) label="pending requests and unread messages"/></span>
            </button>
            <nav class="flex min-h-0 w-full flex-1 flex-col items-center gap-3 overflow-y-auto py-2" aria-label=move || crate::i18n::t("Joined servers")>
                {move ||ui.guilds.get().into_iter().map(move |guild|view!{<GuildButton ui=ui guild= guild/>}).collect_view()}
            </nav>
            <div class="mt-3 flex shrink-0 flex-col items-center gap-3 border-t border-white/10 pt-4">
                <Show when=move ||ui.can_create_server.get()><button class="rail-action" data-tip=move || crate::i18n::t("Create a server") aria-label=move || crate::i18n::t("Create a server") disabled=move ||ui.busy.get() on:click=move |_|open(true)><Icon kind=Glyph::Plus/></button></Show>
                <button class="rail-action" data-tip=move || crate::i18n::t("Join a server") aria-label=move || crate::i18n::t("Join a server") disabled=move ||ui.busy.get() on:click=move |_|open(false)><Icon kind=Glyph::Compass/></button>
                <button class="rail-action" aria-label=move || crate::i18n::t("Refresh servers") data-tip=move || crate::i18n::t("Refresh servers") disabled=move ||ui.busy.get() on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{refresh(ui).await;ui.busy.set(false);});}><Icon kind=Glyph::Refresh/></button>
            </div>
        </aside>
        <dialog node_ref=dialog on:click=move |e|super::dialog::dismiss_outside(e,dialog,ui.busy.get_untracked()) aria-labelledby="server-dialog-title" class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-md overflow-y-auto rounded-2xl border border-white/15 bg-surface p-7 text-white shadow-2xl backdrop:bg-black/70 backdrop:backdrop-blur-sm" on:close=move |_|password.set(String::new()) on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-6 flex items-center justify-between"><h2 id="server-dialog-title" class="text-2xl font-semibold">{move ||if creating.get(){crate::i18n::t("Create a server")}else{crate::i18n::t("Join a server")}}</h2><button aria-label=move || crate::i18n::t("Close dialog") class="px-2 text-xl" disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"×"</button></header>
            <form class="space-y-5" on:submit=submit>
                <Show when=move ||creating.get()><p class="settings-hint">{move ||ui.instance_access.get().map(|a|super::permissions::allocation_summary(&a))}</p>
                    <Show when=quota_reached><p class="settings-hint text-amber-300">{move ||crate::i18n::t("Owned server limit reached; transfer or delete a server, or ask the instance owner to review your limits")}</p></Show>
                    <button type="button" class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{refresh(ui).await;ui.busy.set(false);});}>{move ||crate::i18n::t("Refresh limits")}</button>
                </Show>
                <Show when=move ||creating.get() fallback=move ||view!{<Field label="Server ID" value=server_id/>}><Field label="Server name" value=name/></Show>
                <Field label="Server password (optional)" value=password kind="password" autocomplete="off"/>
                <p class="text-sm text-white/60">{move ||if creating.get(){crate::i18n::t("Leave the password empty to let verified accounts join using your server ID.")}else{crate::i18n::t("Ask the server owner for its ID and password, if it has one.")}}</p>
                <p class="text-sm text-red-300" role="alert">{move ||crate::i18n::message(error.get())}</p>
                <button class="w-full rounded-lg bg-brand px-5 py-3 font-semibold disabled:opacity-50" disabled=move ||ui.busy.get()||(creating.get()&&quota_reached())>{move ||if ui.busy.get(){crate::i18n::t("Please wait…")}else if creating.get(){crate::i18n::t("Create server")}else{crate::i18n::t("Join server")}}</button>
            </form>
        </dialog>
    }
}

#[component]
pub(super) fn ServerHome(ui: Ui) -> impl IntoView {
    let selected = ui.selected_channel;
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let target = RwSignal::new(None::<Guild>);
    let name = RwSignal::new(String::new());
    let voice = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if ui.busy.get_untracked() {
            return;
        }
        let Some(guild) = target.get_untracked() else {
            return;
        };
        let command = PermissionRequest::Change {
            guild_id: guild.id,
            revision: guild.revision,
            change: GuildChange::CreateChannel {
                name: name.get_untracked().trim().to_owned(),
                kind: if voice.get_untracked() {
                    ChannelKind::Voice
                } else {
                    ChannelKind::Text
                },
            },
        };
        error.set(String::new());
        ui.busy.set(true);
        leptos::task::spawn_local(async move {
            let selection = ui.navigation.get_untracked().current();
            let token = ui.token.get_untracked();
            let result = request(ui, command).await;
            // The user may have navigated away while the request was pending.
            if target.try_get_untracked().is_none()
                || !current(ui, selection)
                || ui.token.get_untracked() != token
            {
                ui.busy.set(false);
                return;
            }
            match result {
                Ok(PermissionResponse::State { state }) => {
                    if ui
                        .server
                        .get_untracked()
                        .is_some_and(|s| s.guild.id == guild.id)
                    {
                        let created = state
                            .channels
                            .iter()
                            .find(|c| {
                                ui.server
                                    .get_untracked()
                                    .is_some_and(|s| !s.channels.iter().any(|old| old.id == c.id))
                            })
                            .cloned();
                        refresh(ui).await;
                        if current(ui, selection)
                            && ui.token.get_untracked() == token
                            && selected.try_get_untracked().is_some()
                            && ui
                                .server
                                .get_untracked()
                                .is_some_and(|s| s.guild.id == guild.id)
                            && let Some(created) = created
                            && let Some(channel) = ui.server.get_untracked().and_then(|home| {
                                home.channels.into_iter().find(|channel| {
                                    channel.id == created.id && channel.kind == ChannelKind::Text
                                })
                            })
                        {
                            selected.set(Some(channel));
                        }
                    }
                    if let Some(dialog) = dialog.get() {
                        dialog.close();
                    }
                }
                Ok(_) => error.set("Unexpected server response".into()),
                Err(message) => {
                    error.set(message);
                    refresh(ui).await;
                    if current(ui, selection)
                        && ui.token.get_untracked() == token
                        && target.try_get_untracked().is_some()
                        && let Some(home) =
                            ui.server.get_untracked().filter(|s| s.guild.id == guild.id)
                    {
                        target.set(Some(home.guild));
                    }
                }
            }
            ui.busy.set(false);
        });
    };
    let unread = ui.unread;
    Effect::new(move |_| {
        if let Some(channel) = selected.get()
            && ui
                .server
                .get()
                .is_none_or(|s| !s.channels.iter().any(|c| c.id == channel.id))
        {
            selected.set(None);
        }
    });
    view! {
        <Show when=move ||ui.server.get().is_some() fallback=move ||view!{
            <Show when=move ||ui.navigation.get().current().guild.is_some() fallback=move ||view!{<super::friends::Home ui=ui/>}>
                <section class="flex h-full flex-col items-center justify-center gap-4 p-8 text-center" aria-busy=move ||ui.navigation.get().pending.to_string()>
                    <h2 class="text-lg font-semibold">{move ||ui.guilds.get().into_iter().find(|guild|Some(guild.id)==ui.navigation.get().current().guild).map(|guild|guild.name).unwrap_or_else(||"Server".into())}</h2>
                    <Show when=move ||ui.navigation.get().pending fallback=move ||view!{
                        <p class="text-sm text-red-300" role="alert">{move ||crate::i18n::message(ui.server_error.get())}</p>
                        <button class="rounded-lg bg-brand px-4 py-2 font-semibold" on:click=move |_|if let Some(id)=ui.navigation.get_untracked().current().guild{navigate(ui,id);}>{move || crate::i18n::t("Retry")}</button>
                    }>
                        <p class="text-white/60" role="status" aria-live="polite">{move || crate::i18n::t("Loading server…")}</p>
                    </Show>
                </section>
            </Show>
        }>
            {move ||ui.server.get().map(|home|view!{
                <section class="server-layout">
                    <nav class="channel-sidebar" aria-label=move || crate::i18n::t("Server channels")>
                        <div class="flex items-center gap-2" data-context-server=home.guild.id.to_string()><h2 class="min-w-0 flex-1 break-words text-lg font-bold">{home.guild.name.clone()}</h2>
                            <Show when=move ||home.can_manage_roles><button class="icon-button shrink-0" aria-label=move || crate::i18n::t("Server roles and settings") data-tip=move || crate::i18n::t("Server settings") on:click=move |_|{ui.role_target.set(None);ui.permission_channel.set(None);ui.page.set("permissions");}><Icon kind=Glyph::Settings/></button></Show>
                            <button class="shrink-0 rounded p-1 text-white/50 hover:bg-white/10 hover:text-white" data-tip=move || crate::i18n::t("Copy server ID") aria-label=move || crate::i18n::t("Copy server ID") on:click=move |_| {
                                let id = home.guild.id.to_string();
                                leptos::task::spawn_local(async move {
                                    let result = wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(&id)).await;
                                    ui.status.set(if result.is_ok() {"Server ID copied".into()} else {"Could not copy server ID".into()});
                                });
                            }><svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V4a1 1 0 0 0-1-1H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h4"/></svg></button>
                        </div>
                        <div class="flex items-center justify-between"><h3 class="text-xs font-semibold uppercase tracking-widest text-white/50">{move || crate::i18n::t("Channels")}</h3>
                            <Show when=move ||home.can_manage_channels><button class="rounded px-2 text-xl text-white/60 hover:bg-white/10 hover:text-white" data-tip=move || crate::i18n::t("Create channel") aria-label=move || crate::i18n::t("Create channel") disabled=move ||ui.busy.get() on:click=move |_| {
                                target.set(ui.server.get_untracked().map(|s|s.guild));name.set(String::new());voice.set(false);error.set(String::new());
                                if let Some(dialog)=dialog.get(){let _=dialog.show_modal();}
                            }>"+"</button></Show>
                        </div>
                        <ul class="min-h-0 flex-1 space-y-1 overflow-y-auto text-white/70">{let channel_guild=home.guild.clone();let channels=home.channels;move ||{let mut channels=channels.clone();channels.sort_by_key(|c|!ui.sidebar_preferences.get().pinned.contains(&c.id));let channel_guild=channel_guild.clone();channels.into_iter().map(move |ch|{
                            let id=ch.id; let is_voice=ch.kind==ChannelKind::Voice; let name=ch.name.clone();let guild=channel_guild.clone();let guild_id=guild.id;
                            view!{<li><button data-context-server=guild_id.to_string() data-context-channel=id.to_string() aria-haspopup="menu" class="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left hover:bg-white/10" class:channel-selected=move ||selected.get().is_some_and(|c|c.id==id) class:voice-active=move ||ui.voice.get().channel_id==Some(id) aria-label=name.clone() data-tip=name.clone() disabled=move ||is_voice && ui.voice_joining.get() on:click=move |_|if is_voice {super::audio::join(ui,guild.clone(),ch.clone());}else{selected.set(Some(ch.clone()));}>
                                {move || if is_voice {view!{<Icon kind=Glyph::Volume/>}.into_any()}else{view!{<span class="w-5 text-center text-xl text-white/40">"#"</span>}.into_any()}}
                                <span class="min-w-0 flex-1 truncate text-sm">{name.clone()}</span>
                                <Show when=move ||ui.sidebar_preferences.get().pinned.contains(&id)><span aria-label=move || crate::i18n::t("Pinned") class="channel-indicator"><Icon kind=Glyph::Pin/></span></Show>
                                <Show when=move ||super::notifications::muted(ui, thiscord_shared::notifications::Target::Channel { guild_id, channel_id: id })><span aria-label=move || crate::i18n::t("Notifications muted") class="channel-indicator"><Icon kind=Glyph::BellOff/></span></Show>
                                <span class="shrink-0 text-xs text-brand">{move||if super::notifications::muted(ui, thiscord_shared::notifications::Target::Channel { guild_id, channel_id: id }){None}else{unread.get().iter().find(|u|u.channel_id==id&&u.count>0).map(|u|format!("{}{}",u.count,if u.mentions>0{" @"}else{""}))}}</span>
                            </button>{is_voice.then(||view!{<VoiceRoster ui=ui guild=guild_id channel=id/>})}</li>}
                        }).collect_view()}}</ul>
                    </nav>
                    <Show when=move||selected.get().is_some() fallback=move||view!{<div class="flex items-center justify-center rounded-xl border border-white/10 p-8 text-white/60">{move || crate::i18n::t("Choose a channel to get started.")}</div>}>
                        {move||selected.get().map(|ch|if ch.kind==ChannelKind::Voice {view!{<super::audio::VoiceChannel ui=ui guild=home.guild.id channel=ch.id name=ch.name/>}.into_any()}else{view!{<super::chat::ChatPanel ui=ui guild=home.guild.id channel=ch.id name=ch.name/>}.into_any()})}
                    </Show>
                </section>
            })}
        </Show>
        <dialog node_ref=dialog on:click=move |e|super::dialog::dismiss_outside(e,dialog,ui.busy.get_untracked()) aria-labelledby="channel-dialog-title" class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-md overflow-y-auto rounded-2xl border border-white/15 bg-surface p-6 text-white shadow-2xl backdrop:bg-black/70" on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-5 flex items-center justify-between"><h2 id="channel-dialog-title" class="text-xl font-semibold">{move || crate::i18n::t("Create channel")}</h2><button aria-label=move || crate::i18n::t("Close dialog") disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"×"</button></header>
            <form class="space-y-5" on:submit=submit>
                <Field label="Channel name" value=name/>
                <label class="block space-y-2"><span>{move || crate::i18n::t("Channel type")}</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move ||if voice.get(){"voice"}else{"text"} on:change=move |ev|voice.set(event_target_value(&ev)=="voice")><option value="text">{move || crate::i18n::t("Text")}</option><option value="voice">{move || crate::i18n::t("Voice")}</option></select></label>
                <p class="text-sm text-red-300" role="alert">{move ||crate::i18n::message(error.get())}</p>
                <button class="w-full rounded-lg bg-brand px-5 py-3 font-semibold disabled:opacity-50" disabled=move ||ui.busy.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Create channel")}</button>
            </form>
        </dialog>
    }
}
