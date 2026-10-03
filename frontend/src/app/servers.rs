use super::{Field, Ui};
use leptos::prelude::*;
use thiscord_shared::{GuildId, permissions::*};

async fn request(ui: Ui, command: PermissionRequest) -> Result<PermissionResponse, String> {
    let token = ui.token.get_untracked();
    let response =
        crate::account_client::api_request(PERMISSIONS_PATH, &command, token.as_deref()).await;
    if token != ui.token.get_untracked() {
        return Err("Session changed; refresh your servers".into());
    }
    response
}

pub(super) async fn refresh(ui: Ui) {
    match request(ui, PermissionRequest::ListGuilds {}).await {
        Ok(PermissionResponse::Guilds { guilds }) => {
            if ui
                .server
                .get_untracked()
                .is_some_and(|s| !guilds.iter().any(|g| g.id == s.guild.id))
            {
                ui.server.set(None);
            }
            ui.guilds.set(guilds);
            if let Some(current) = ui.server.get_untracked() {
                match request(
                    ui,
                    PermissionRequest::ViewGuild {
                        guild_id: current.guild.id,
                    },
                )
                .await
                {
                    Ok(PermissionResponse::Home { home }) => ui.server.set(Some(home)),
                    _ => ui.server.set(None),
                }
            }
        }
        Err(error) => ui.status.set(error),
        _ => {}
    }
    if let Ok(PermissionResponse::Instance { access }) =
        request(ui, PermissionRequest::Instance {}).await
    {
        ui.can_create_server.set(
            access.role != InstanceRole::User
                && ui.account.get_untracked().is_some_and(|a| a.email_verified),
        );
    } else {
        ui.can_create_server.set(false);
    }
}

async fn select(ui: Ui, id: GuildId) -> Result<(), String> {
    let PermissionResponse::Home { home } =
        request(ui, PermissionRequest::ViewGuild { guild_id: id }).await?
    else {
        return Err("Unexpected server response".into());
    };
    ui.server.set(Some(home));
    ui.page.set("servers");
    Ok(())
}

#[component]
pub(super) fn ServerRail(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let creating = RwSignal::new(true);
    let name = RwSignal::new(String::new());
    let server_id = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
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
            match request(ui, command).await {
                Ok(response) => {
                    let id = match response {
                        PermissionResponse::State { state } => Some(state.guild.id),
                        PermissionResponse::Joined { guild } => Some(guild.id),
                        _ => None,
                    };
                    refresh(ui).await;
                    if let Some(id) = id
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
                    format!("{message}. Check the server ID and password and verify your email.")
                }),
            }
            ui.busy.set(false);
        });
    };
    view! {
        <aside class="fixed inset-y-0 left-0 z-20 flex w-20 flex-col items-center border-r border-white/10 bg-slate-950 py-4" aria-label="Servers">
            <button class="mb-4 h-12 w-12 shrink-0 rounded-2xl bg-brand text-xl font-bold" title="Thiscord home" aria-label="Thiscord home" on:click=move |_|{ui.server.set(None);ui.page.set("servers");}>"T"</button>
            <nav class="flex min-h-0 w-full flex-1 flex-col items-center gap-3 overflow-y-auto py-2" aria-label="Joined servers">
                {move ||ui.guilds.get().into_iter().map(move |guild|{
                    let id=guild.id;let initials=guild.name.split_whitespace().take(2).filter_map(|s|s.chars().next()).collect::<String>().to_uppercase();
                    view!{<button class="h-12 w-12 shrink-0 overflow-hidden rounded-full border-2 border-transparent bg-slate-700 transition hover:border-white/70 focus-visible:outline-2 focus-visible:outline-white disabled:opacity-50" class:border-white=move ||ui.server.get().is_some_and(|s|s.guild.id==id) title=guild.name.clone() aria-label=guild.name disabled=move ||ui.busy.get()
                        on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{if let Err(e)=select(ui,id).await{ui.status.set(e);refresh(ui).await;}ui.busy.set(false);});}>
                        <svg viewBox="0 0 48 48" role="img" aria-hidden="true"><circle cx="24" cy="24" r="24" fill="#5865f2"/><text x="24" y="25" text-anchor="middle" dominant-baseline="middle" fill="white" font-size="16" font-weight="600">{initials}</text></svg>
                    </button>}
                }).collect_view()}
            </nav>
            <div class="mt-3 flex shrink-0 flex-col items-center gap-3 border-t border-white/10 pt-4">
                <Show when=move ||ui.can_create_server.get()><button class="h-12 w-12 rounded-full bg-white/10 text-3xl text-emerald-300 hover:bg-emerald-500/20 disabled:opacity-50" title="Create a server" aria-label="Create a server" disabled=move ||ui.busy.get() on:click=move |_|open(true)>"+"</button></Show>
                <button class="h-12 w-12 rounded-full bg-white/10 text-xs font-semibold text-emerald-300 hover:bg-emerald-500/20 disabled:opacity-50" title="Join a server" aria-label="Join a server" disabled=move ||ui.busy.get() on:click=move |_|open(false)>"Join"</button>
                <button class="h-10 w-12 text-xs text-white/60 hover:text-white disabled:opacity-50" title="Refresh servers" disabled=move ||ui.busy.get() on:click=move |_|{ui.busy.set(true);leptos::task::spawn_local(async move{refresh(ui).await;ui.busy.set(false);});}>"Refresh"</button>
                <button class="h-10 w-12 text-xs text-white/60 hover:text-white" title="Account settings" aria-label="Account settings" on:click=move |_|ui.page.set("profile")>"Settings"</button>
            </div>
        </aside>
        <dialog node_ref=dialog aria-labelledby="server-dialog-title" class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-md overflow-y-auto rounded-2xl border border-white/15 bg-surface p-7 text-white shadow-2xl backdrop:bg-black/70 backdrop:backdrop-blur-sm" on:close=move |_|password.set(String::new()) on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-6 flex items-center justify-between"><h2 id="server-dialog-title" class="text-2xl font-semibold">{move ||if creating.get(){"Create a server"}else{"Join a server"}}</h2><button aria-label="Close dialog" class="px-2 text-xl" disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"×"</button></header>
            <form class="space-y-5" on:submit=submit>
                <Show when=move ||creating.get() fallback=move ||view!{<Field label="Server ID" value=server_id/>}><Field label="Server name" value=name/></Show>
                <Field label="Server password (optional)" value=password kind="password" autocomplete="off"/>
                <p class="text-sm text-white/60">{move ||if creating.get(){"Leave the password empty to let verified accounts join using your server ID."}else{"Ask the server owner for its ID and password, if it has one."}}</p>
                <p class="text-sm text-red-300" role="alert">{move ||error.get()}</p>
                <button class="w-full rounded-lg bg-brand px-5 py-3 font-semibold disabled:opacity-50" disabled=move ||ui.busy.get()>{move ||if ui.busy.get(){"Please wait…"}else if creating.get(){"Create server"}else{"Join server"}}</button>
            </form>
        </dialog>
    }
}

#[component]
pub(super) fn ServerHome(ui: Ui) -> impl IntoView {
    let selected = RwSignal::new(None::<Channel>);
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
            let result = request(ui, command).await;
            // The user may have navigated away while the request was pending.
            if target.try_get_untracked().is_none() {
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
                        if selected.try_get_untracked().is_some()
                            && ui
                                .server
                                .get_untracked()
                                .is_some_and(|s| s.guild.id == guild.id)
                            && let Some(channel) = created.filter(|c| c.kind == ChannelKind::Text)
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
                    if target.try_get_untracked().is_some()
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
            <section class="flex h-full min-h-0 flex-col items-center justify-center gap-4 text-center"><h2 class="text-3xl font-bold">"Welcome to Thiscord"</h2><p class="max-w-md text-white/60">"Choose a server on the left, or join one with its server ID."</p><Show when=move ||ui.can_create_server.get()><p class="text-white/60">"Use + to create your own server."</p></Show></section>
        }>
            {move ||ui.server.get().map(|home|view!{
                <section class="grid h-full min-h-0 min-w-0 grid-cols-[160px_minmax(0,1fr)] gap-3 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-5">
                    <nav class="flex min-h-0 min-w-0 flex-col gap-5 rounded-xl bg-white/5 p-3 lg:p-4" aria-label="Server channels">
                        <div class="flex items-center gap-2"><h2 class="min-w-0 flex-1 break-words text-lg font-bold">{home.guild.name.clone()}</h2>
                            <button class="shrink-0 rounded p-1 text-white/50 hover:bg-white/10 hover:text-white" title=format!("Copy server ID: {}",home.guild.id) aria-label="Copy server ID" on:click=move |_| {
                                let id = home.guild.id.to_string();
                                leptos::task::spawn_local(async move {
                                    let result = wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(&id)).await;
                                    ui.status.set(if result.is_ok() {"Server ID copied".into()} else {"Could not copy server ID".into()});
                                });
                            }><svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V4a1 1 0 0 0-1-1H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h4"/></svg></button>
                        </div>
                        <div class="flex items-center justify-between"><h3 class="text-xs font-semibold uppercase tracking-widest text-white/50">"Channels"</h3>
                            <Show when=move ||home.can_manage_channels><button class="rounded px-2 text-xl text-white/60 hover:bg-white/10 hover:text-white" title="Create channel" aria-label="Create channel" disabled=move ||ui.busy.get() on:click=move |_| {
                                target.set(ui.server.get_untracked().map(|s|s.guild));name.set(String::new());voice.set(false);error.set(String::new());
                                if let Some(dialog)=dialog.get(){let _=dialog.show_modal();}
                            }>"+"</button></Show>
                        </div>
                        <ul class="min-h-0 flex-1 space-y-2 overflow-y-auto text-white/70">{home.channels.into_iter().map(move |ch|{let id=ch.id;let label=format!("{} {}",if ch.kind==ChannelKind::Voice{"◖"}else{"#"},ch.name);view!{<li><button class="flex w-full items-center justify-between gap-2 rounded p-2 text-left hover:bg-white/10 disabled:opacity-40" on:click=move |_|selected.set(Some(ch.clone()))><span class="truncate">{label}</span><span class="shrink-0 text-xs text-brand">{move||unread.get().iter().find(|u|u.channel_id==id&&u.count>0).map(|u|format!("{}{}",u.count,if u.mentions>0{" @"}else{""}))}</span></button></li>}}).collect_view()}</ul>
                        <Show when=move ||home.can_manage_roles><button class="text-sm text-brand underline" on:click=move |_|ui.page.set("permissions")>"Server roles & settings"</button></Show>
                    </nav>
                    <Show when=move||selected.get().is_some() fallback=move||view!{<div class="flex items-center justify-center rounded-xl border border-white/10 p-8 text-white/60">"Choose a text channel to start chatting."</div>}>
                        {move||selected.get().map(|ch|if ch.kind==ChannelKind::Voice {view!{<super::audio::VoiceChannel ui=ui guild=home.guild.id channel=ch.id name=ch.name/>}.into_any()}else{view!{<super::chat::ChatPanel ui=ui guild=home.guild.id channel=ch.id name=ch.name/>}.into_any()})}
                    </Show>
                </section>
            })}
        </Show>
        <dialog node_ref=dialog aria-labelledby="channel-dialog-title" class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-md overflow-y-auto rounded-2xl border border-white/15 bg-surface p-6 text-white shadow-2xl backdrop:bg-black/70" on:cancel=move |ev: web_sys::Event|{if ui.busy.get_untracked(){ev.prevent_default();}}>
            <header class="mb-5 flex items-center justify-between"><h2 id="channel-dialog-title" class="text-xl font-semibold">"Create channel"</h2><button aria-label="Close dialog" disabled=move ||ui.busy.get() on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"×"</button></header>
            <form class="space-y-5" on:submit=submit>
                <Field label="Channel name" value=name/>
                <label class="block space-y-2"><span>"Channel type"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move ||if voice.get(){"voice"}else{"text"} on:change=move |ev|voice.set(event_target_value(&ev)=="voice")><option value="text">"Text"</option><option value="voice">"Voice"</option></select></label>
                <p class="text-sm text-red-300" role="alert">{move ||error.get()}</p>
                <button class="w-full rounded-lg bg-brand px-5 py-3 font-semibold disabled:opacity-50" disabled=move ||ui.busy.get()||name.get().trim().is_empty()>"Create channel"</button>
            </form>
        </dialog>
    }
}
