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
    ui.invites.clear_links();
    home(ui);
    ui.server_refresh
        .update(|generation| *generation = generation.wrapping_add(1));
}

fn update_home(ui: Ui, selection: Selection, home: GuildHome) {
    if !current(ui, selection) || selection.guild != Some(home.guild.id) {
        return;
    }
    let refreshed = thiscord_frontend::navigation::refresh_channel(
        ui.selected_channel.get_untracked().as_ref(),
        &home.channels,
    );
    ui.selected_channel.set(refreshed);
    let unchanged = ui.server.with_untracked(|current| {
        current.as_ref().is_some_and(|old| {
            old.guild.id == home.guild.id
                && old.guild.revision == home.guild.revision
                && old.guild.name == home.guild.name
                && old.guild.icon_id == home.guild.icon_id
                && old.guild.owner == home.guild.owner
                && old.can_manage_roles == home.can_manage_roles
                && old.can_manage_channels == home.can_manage_channels
                && old.can_manage_invites == home.can_manage_invites
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

pub(super) fn navigate(ui: Ui, id: GuildId) {
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
fn GuildButton(ui: Ui, guild: Guild, roving: super::keyboard::Roving<GuildId>) -> impl IntoView {
    let id = guild.id;
    let (name, icon) = (guild.name.clone(), guild.icon_id);
    let current = move || ui.navigation.with(|n| n.current().guild == Some(id));
    view! {
        <li><button class="guild-button relative overflow-hidden" data-rove data-context-server=id.to_string() aria-haspopup="menu" class:selected=current
            aria-current=move || current().then_some("page") tabindex=roving.tabindex(id) on:focus=move |_| roving.focus(id)
            aria-busy=move ||(current() && ui.navigation.get().pending).to_string() aria-label=guild.name.clone() data-tip=guild.name
            on:click=move |_|navigate(ui,id)><crate::avatar::GuildIcon name=name icon_id=icon/></button></li>
    }
}

#[component]
fn VoiceRoster(ui: Ui, guild: GuildId, channel: thiscord_shared::ChannelId) -> impl IntoView {
    let speaking = Memo::new(move |_| {
        // Borrow both statuses: they are replaced on every poll, and cloning
        // them for each voice channel's roster dominated this memo.
        ui.voice.with(|voice| {
            ui.audio_status.with(|audio| {
                thiscord_frontend::voice_overlay::speaking_in_channel(
                    voice,
                    audio.as_ref(),
                    channel,
                )
            })
        })
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
            view!{<li tabindex="0" aria-haspopup="menu" data-context-server=guild.to_string() data-context-channel=channel.to_string() data-context-member=id.to_string() class="voice-member sidebar-member" class:voice-speaking=move ||speaking.get().contains(&id) data-tip=move ||{let tip=format!("{} (@{})",member.get().display_name(),member.get().username);match ui.members.status(guild,id) {Some(status)=>format!("{tip} · {status}"),None=>tip}}>
                <button class="voice-person" class:has-stream=move ||member.get().sharing_screen || member.get().camera_active aria-label=move ||if member.get().sharing_screen || member.get().camera_active{thiscord_frontend::localized_format!(crate::i18n::language(), "Watch {}’s stream",member.get().display_name())}else{member.get().display_name().to_owned()} data-tip=move ||if member.get().sharing_screen || member.get().camera_active{crate::i18n::t("Watch stream")}else{crate::i18n::t("Right-click for options")} on:click=move |_|if member.get_untracked().sharing_screen || member.get_untracked().camera_active{super::screen::open_stream(ui,guild,channel,member.get_untracked());}>
                <span class="voice-avatar"><crate::avatar::Avatar class="size-6 text-xs" name=Signal::derive(move ||member.get().display_name().to_owned()) avatar_id=Signal::derive(move ||member.get().avatar_id)/></span>
                <span class="min-w-0 flex-1 truncate">{move ||member.get().display_name().to_owned()}</span>
                </button><Show when=move ||speaking.get().contains(&id)><span class="sr-only">{move || crate::i18n::t("Speaking")}</span></Show>
                <Show when=move ||member.get().is_deafened()><span role="img" aria-label=move || crate::i18n::t("Deafened")><Icon kind=Glyph::Deafen/></span></Show>
                <Show when=move ||!member.get().is_deafened() && (member.get().is_muted() || !member.get().can_speak)><span role="img" aria-label=move || crate::i18n::t("Muted")><Icon kind=Glyph::MicOff/></span></Show>
                <Show when=move ||member.get().sharing_screen || member.get().camera_active><button class="stream-live-badge" aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Watch {}’s stream",member.get().display_name()) data-tip=move || crate::i18n::t("Watch stream") on:click=move |_|super::screen::open_stream(ui,guild,channel,member.get_untracked())>{move || crate::i18n::t("LIVE")}</button></Show>
            </li>}
        }/>
    </ul>}
}

/// Screen-reader name for a sidebar channel: its name, kind, unread count,
/// mentions and voice connection (the visual badges are not announced).
fn channel_label(
    ui: Ui,
    guild: GuildId,
    channel: thiscord_shared::ChannelId,
    name: &str,
    voice: bool,
) -> String {
    let mut parts = vec![
        name.to_owned(),
        crate::i18n::t(if voice {
            "Voice channel"
        } else {
            "Text channel"
        })
        .to_owned(),
    ];
    let muted = super::notifications::muted(
        ui,
        thiscord_shared::notifications::Target::Channel {
            guild_id: guild,
            channel_id: channel,
        },
    );
    if muted {
        parts.push(crate::i18n::t("Notifications muted").to_owned());
    } else if let Some((count, mentions)) = ui.unread.with(|unread| {
        unread
            .iter()
            .find(|u| u.channel_id == channel && u.count > 0)
            .map(|u| (u.count, u.mentions))
    }) {
        parts.push(thiscord_frontend::localized_format!(
            crate::i18n::language(),
            "{} unread",
            if count >= thiscord_shared::chat::UNREAD_COUNT_LIMIT {
                "999+".to_owned()
            } else {
                count.to_string()
            }
        ));
        if mentions > 0 {
            parts.push(crate::i18n::t("mentions you").to_owned());
        }
    }
    if voice && ui.voice.with(|v| v.channel_id == Some(channel)) {
        parts.push(crate::i18n::t("Connected").to_owned());
    }
    parts.join(", ")
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
            super::dialog::open(&dialog);
        }
    };
    Effect::new(move |_| {
        let ready = ui.account.get().is_some_and(|a| a.email_verified) && !ui.busy.get();
        if let Some(code) = ui.invites.pending.get()
            && ready
            && let Some(dialog) = dialog.get()
        {
            creating.set(false);
            server_id.set(code);
            password.set(String::new());
            error.set(String::new());
            if super::dialog::open(&dialog) {
                ui.invites.pending.set(None);
                super::invites::clear_fragment();
            }
        }
    });
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
        } else if !server_id.get_untracked().trim().is_empty()
            && server_id.get_untracked().trim().parse::<GuildId>().is_err()
        {
            let supplied = server_id.get_untracked();
            let Some(code) =
                thiscord_frontend::invites::input_code(&supplied, &super::invites::base())
            else {
                error.set(
                    "Use a complete invite link or code from this Thiscord installation.".into(),
                );
                return;
            };
            PermissionRequest::AcceptInvite { code }
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
                        "{message}. Check the invite or server ID and verify your email.",
                        message = crate::i18n::message(message)
                    )
                }),
            }
            ui.busy.set(false);
        });
    };
    let roving = super::keyboard::Roving::new(
        move || {
            ui.guilds
                .with(|guilds| guilds.iter().map(|g| g.id).collect())
        },
        move || ui.navigation.with(|n| n.current().guild),
    );
    view! {
        <nav class="server-rail" aria-label=move || crate::i18n::t("Servers")>
            <button class="home-button relative mb-3 flex h-12 w-12 shrink-0 items-center justify-center" aria-current=move || ui.navigation.with(|n| n.current().guild.is_none()).then_some("page") data-tip=move || crate::i18n::t("Thiscord home") aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "Thiscord home, {} incoming requests, {} unread messages",ui.friends.pending_count(),super::friends::count_text(ui.friends.unread_count())) on:click=move |_|{ui.friends.selected.set(None);home(ui);}>
                <crate::brand::BrandMark class="h-8 w-8"/>
                <span class="absolute -bottom-1 -right-1"><super::friends::CountBubble count=Signal::derive(move ||ui.friends.pending_count()+ui.friends.unread_count()) label="pending requests and unread messages"/></span>
            </button>
            <ul class="flex min-h-0 w-full flex-1 flex-col items-center gap-2 overflow-y-auto border-t border-white/10 py-3" aria-label=move || crate::i18n::t("Joined servers")
                data-rove-list on:keydown=move |e| super::keyboard::rove(&e)>
                {move ||ui.guilds.get().into_iter().map(move |guild|view!{<GuildButton ui=ui guild=guild roving/>}).collect_view()}
            </ul>
            <div class="mt-2 flex shrink-0 flex-col items-center gap-2 border-t border-white/10 pt-3">
                <Show when=move ||ui.can_create_server.get()><button class="rail-action" data-tip=move || crate::i18n::t("Create a server") aria-label=move || crate::i18n::t("Create a server") disabled=move ||ui.busy.get() on:click=move |_|open(true)><Icon kind=Glyph::Plus/></button></Show>
                <button class="rail-action" data-tip=move || crate::i18n::t("Join a server") aria-label=move || crate::i18n::t("Join a server") disabled=move ||ui.busy.get() on:click=move |_|open(false)><Icon kind=Glyph::Compass/></button>
                <button class="rail-action" aria-label=move || crate::i18n::t("Refresh servers") data-tip=move || crate::i18n::t("Refresh servers") disabled=move ||ui.busy.get() on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{refresh(ui).await;ui.busy.set(false);});}><Icon kind=Glyph::Refresh/></button>
            </div>
        </nav>
        <dialog node_ref=dialog on:click=move |e|super::dialog::dismiss_outside(e,dialog,ui.busy.get_untracked()) aria-labelledby="server-dialog-title" class="app-dialog max-w-md" on:close=move |_|password.set(String::new()) on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-6 flex items-start justify-between gap-4"><span class="dialog-mark"><Icon kind=Glyph::Server/></span><h2 id="server-dialog-title" class="dialog-title mr-auto self-center">{move ||if creating.get(){crate::i18n::t("Create a server")}else{crate::i18n::t("Join a server")}}</h2><button aria-label=move || crate::i18n::t("Close dialog") class="icon-button -mr-2" disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}><Icon kind=Glyph::Close/></button></header>
            <form class="space-y-5" on:submit=submit>
                <Show when=quota_reached><p class="dialog-notice" role="status">{move ||crate::i18n::t("Owned server limit reached; transfer or delete a server, or ask the instance owner to review your limits")}</p></Show>
                <Show when=move ||creating.get() fallback=move ||view!{<Field label="Server ID, invite code or invite link" value=server_id/>}><Field label="Server name" value=name/></Show>
                <Show when=move ||{creating.get() || server_id.get().trim().parse::<GuildId>().is_ok()}><Field label="Server password (optional)" value=password kind="password" autocomplete="off"/></Show>
                <p class="dialog-description">{move ||if creating.get(){crate::i18n::t("Leave the password empty to let verified accounts join using your server ID.")}else{crate::i18n::t("Paste an invite link to join. No server password is needed for an invite.")}}</p>
                <p class="composer-error" role="alert">{move ||crate::i18n::message(error.get())}</p>
                <button class="btn btn-primary btn-block" disabled=move ||ui.busy.get()||(creating.get()&&(quota_reached()||name.get().trim().is_empty()))||(!creating.get()&&server_id.get().trim().is_empty())>{move ||if ui.busy.get(){crate::i18n::t("Please wait…")}else if creating.get(){crate::i18n::t("Create server")}else{crate::i18n::t("Join server")}}</button>
                <Show when=move ||creating.get()><p class="dialog-footnote"><span>{move ||ui.instance_access.get().map(|a|super::permissions::allocation_summary(&a))}</span>" "<button type="button" class="text-link" disabled=move ||ui.busy.get() on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{refresh(ui).await;ui.busy.set(false);});}>{move ||crate::i18n::t("Refresh limits")}</button></p></Show>
            </form>
        </dialog>
    }
}

/// Phones show either the channel list or the open channel; this returns to
/// the list without closing the channel. Hidden at tablet widths and up.
#[component]
pub(super) fn BackButton(
    ui: Ui,
    #[prop(default = "Back to channels")] label: &'static str,
) -> impl IntoView {
    view! {<button type="button" class="icon-button mobile-only" aria-label=move || crate::i18n::t(label) on:click=move |_|ui.mobile_content.set(false)><Icon kind=Glyph::Back/></button>}
}

#[component]
fn MobileBack(ui: Ui) -> impl IntoView {
    view! {<div class="mobile-back-bar mobile-only"><BackButton ui=ui/><span>{move || crate::i18n::t("Channels")}</span></div>}
}

#[component]
pub(super) fn ServerHome(ui: Ui) -> impl IntoView {
    let selected = ui.selected_channel;
    // Only identity changes remount a panel. Metadata refreshes keep its owner,
    // draft, history requests and voice controls alive.
    let panel = Memo::new(move |_| {
        thiscord_frontend::navigation::channel_identity(
            ui.server.get().map(|home| home.guild.id),
            selected.get().as_ref(),
        )
    });
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
    let channel_roving = super::keyboard::Roving::new(
        move || {
            ui.server.with(|home| {
                home.as_ref().map_or_else(Vec::new, |home| {
                    ui.sidebar_preferences.with(|p| {
                        thiscord_frontend::keyboard::channel_order(
                            &home.channels,
                            &home.categories,
                            &p.pinned,
                        )
                        .into_iter()
                        .map(|c| c.id)
                        .collect()
                    })
                })
            })
        },
        move || selected.with(|c| c.as_ref().map(|c| c.id)),
    );
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
            <section class="server-layout">
                {move ||ui.server.get().map(|home|view!{
                    <nav class="channel-sidebar" aria-label=move || crate::i18n::t("Server channels")>
                        <div class="sidebar-header" data-context-server=home.guild.id.to_string()><span class="relative grid size-8 shrink-0 place-items-center overflow-hidden rounded-[10px] bg-raised text-xs font-semibold"><crate::avatar::GuildIcon name=home.guild.name.clone() icon_id=home.guild.icon_id/></span><h2 class="min-w-0 flex-1 truncate">{home.guild.name.clone()}</h2>
                            <Show when=move ||home.can_manage_bots || home.can_manage_roles || home.can_manage_channels || home.can_manage_invites || home.can_moderate><button class="icon-button shrink-0" aria-label=move || crate::i18n::t("Server roles and settings") data-tip=move || crate::i18n::t("Server settings") on:click=move |_|{ui.role_target.set(None);ui.permission_channel.set(None);ui.page.set(if home.can_manage_roles{"permissions"}else if home.can_manage_bots{"server_bots"}else if home.can_manage_channels{"server_organization"}else if home.can_manage_invites{"server_invites"}else{"moderation"});}><Icon kind=Glyph::Settings/></button></Show>
                            <Show when=move ||home.can_manage_invites><button class="icon-button shrink-0" data-tip=move || crate::i18n::t("Copy invite link") aria-label=move || crate::i18n::t("Copy invite link") disabled=move ||ui.busy.get() on:click=move |_|super::invites::copy(ui,home.guild.id)><Icon kind=Glyph::Link/></button></Show>
                        </div>
                        <div class="-mb-2 flex items-center justify-between pl-2"><h3 class="sidebar-section-title">{move || crate::i18n::t("Channels")}</h3>
                            <Show when=move ||home.can_manage_channels><button class="icon-button shrink-0" data-tip=move || crate::i18n::t("Create channel") aria-label=move || crate::i18n::t("Create channel") disabled=move ||ui.busy.get() on:click=move |_| {
                                target.set(ui.server.get_untracked().map(|s|s.guild));name.set(String::new());voice.set(false);error.set(String::new());
                                if let Some(dialog)=dialog.get(){super::dialog::open(&dialog);}
                            }><Icon kind=Glyph::Plus/></button></Show>
                        </div>
                        <ul class="min-h-0 flex-1 space-y-0.5 overflow-y-auto" data-rove-list on:keydown=move |e| super::keyboard::rove(&e)>{let channel_guild=home.guild.clone();let channels=home.channels;let categories=home.categories;move ||{let channels=ui.sidebar_preferences.with(|p|thiscord_frontend::keyboard::channel_order(&channels,&categories,&p.pinned).into_iter().cloned().collect::<Vec<_>>());let mut previous=None;let categories=categories.clone();let channel_guild=channel_guild.clone();channels.into_iter().map(move |ch|{
                            let id=ch.id; let is_voice=ch.kind==ChannelKind::Voice; let name=ch.name.clone();let guild=channel_guild.clone();let guild_id=guild.id;
                            let heading=if previous!=Some(ch.category_id){previous=Some(ch.category_id);Some(ch.category_id.and_then(|id|categories.iter().find(|cat|cat.id==id).map(|cat|cat.name.clone())).unwrap_or_else(||crate::i18n::t("No category").into()))}else{None};
                            view!{<li>{heading.map(|title|view!{<h4 class="sidebar-section-title px-2 pb-1.5 pt-4">{title}</h4>})}<button data-rove tabindex=channel_roving.tabindex(id) on:focus=move |_|channel_roving.focus(id) data-context-server=guild_id.to_string() data-context-channel=id.to_string() aria-haspopup="menu" class="channel-item" class:channel-unread=move ||!super::notifications::muted(ui, thiscord_shared::notifications::Target::Channel { guild_id, channel_id: id }) && unread.with(|unread|unread.iter().any(|u|u.channel_id==id&&u.count>0)) class:channel-selected=move ||selected.get().is_some_and(|c|c.id==id) class:voice-active=move ||ui.voice.get().channel_id==Some(id)
                                aria-current=move ||selected.with(|c|c.as_ref().is_some_and(|c|c.id==id)).then_some("page")
                                aria-label={let name=name.clone();move ||channel_label(ui,guild_id,id,&name,is_voice)} data-tip=name.clone() data-tip-overflow="" disabled=move ||is_voice && ui.voice_joining.get() on:click=move |_|if is_voice {if selected.with_untracked(|c|c.as_ref().is_some_and(|c|c.id==id)){ui.mobile_content.set(true);}super::audio::join(ui,guild.clone(),ch.clone());}else{selected.set(Some(ch.clone()));ui.mobile_content.set(true);}>
                                {move || if is_voice {view!{<span class="channel-glyph"><Icon kind=Glyph::Voice/></span>}.into_any()}else{view!{<span class="channel-glyph text-base font-semibold leading-none" aria-hidden="true">"#"</span>}.into_any()}}
                                <span class="min-w-0 flex-1 truncate">{name.clone()}</span>
                                <Show when=move ||ui.sidebar_preferences.get().pinned.contains(&id)><span aria-label=move || crate::i18n::t("Pinned") class="channel-indicator"><Icon kind=Glyph::Pin/></span></Show>
                                <Show when=move ||super::notifications::muted(ui, thiscord_shared::notifications::Target::Channel { guild_id, channel_id: id })><span aria-label=move || crate::i18n::t("Notifications muted") class="channel-indicator"><Icon kind=Glyph::BellOff/></span></Show>
                                {move||if super::notifications::muted(ui, thiscord_shared::notifications::Target::Channel { guild_id, channel_id: id }){None}else{unread.with(|unread|unread.iter().find(|u|u.channel_id==id&&u.count>0).map(|u|(if u.count>=thiscord_shared::chat::UNREAD_COUNT_LIMIT{"999+".into()}else{u.count.to_string()},u.mentions>0)))}.map(|(count,mention)|view!{<span class="channel-badge" class:channel-badge-mention=mention>{count}{mention.then_some(" @")}</span>})}
                            </button>{is_voice.then(||view!{<VoiceRoster ui=ui guild=guild_id channel=id/>})}</li>}
                        }).collect_view()}}</ul>
                    </nav>
                })}
                <div class="server-content">
                    <Show when=move||selected.get().is_some() fallback=move||view!{<div class="flex h-full flex-col"><div class="pane-header" aria-hidden="true"></div><div class="empty-state flex-1"><span class="empty-state-icon"><Icon kind=Glyph::Hash/></span><p>{move || crate::i18n::t("Choose a channel to get started.")}</p></div></div>}>
                        <For each={move ||panel.get().into_iter().collect::<Vec<_>>()} key=|(guild, channel, _)|(*guild, *channel) children=move |(guild, channel, kind)| {
                            let name = Signal::derive(move || selected.get().filter(|ch|ch.id==channel).map(|ch|ch.name).unwrap_or_default());
                            if kind==ChannelKind::Voice {view!{<MobileBack ui=ui/><super::audio::VoiceChannel ui=ui guild=guild channel=channel name=name/>}.into_any()}else{view!{<super::chat::ChatPanel ui=ui guild=guild channel=channel name=name/>}.into_any()}
                        }/>

                    </Show>
                </div>
            </section>
        </Show>
        <dialog node_ref=dialog on:click=move |e|super::dialog::dismiss_outside(e,dialog,ui.busy.get_untracked()) aria-labelledby="channel-dialog-title" class="app-dialog max-w-md" on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-5 flex items-center justify-between"><h2 id="channel-dialog-title" class="dialog-title">{move || crate::i18n::t("Create channel")}</h2><button class="icon-button -mr-2" aria-label=move || crate::i18n::t("Close dialog") disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}><Icon kind=Glyph::Close/></button></header>
            <form class="space-y-5" on:submit=submit>
                <Field label="Channel name" value=name/>
                <label class="field"><span class="field-label">{move || crate::i18n::t("Channel type")}</span><select class="field-input" prop:value=move ||if voice.get(){"voice"}else{"text"} on:change=move |ev|voice.set(event_target_value(&ev)=="voice")><option value="text">{move || crate::i18n::t("Text")}</option><option value="voice">{move || crate::i18n::t("Voice")}</option></select></label>
                <p class="text-sm text-red-300" role="alert">{move ||crate::i18n::message(error.get())}</p>
                <button class="btn btn-primary btn-block" disabled=move ||ui.busy.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Create channel")}</button>
            </form>
        </dialog>
    }
}
