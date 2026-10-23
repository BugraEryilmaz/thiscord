use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{AccountId, GuildId, permissions::*};

fn audit_description(entry: &AuditEntry) -> String {
    let details: serde_json::Value = serde_json::from_str(&entry.details).unwrap_or_default();
    let label = match entry.action.as_str() {
        "remove_member" => "Kick from server",
        "ban_member" => "Ban from server",
        "unban_member" => "Unban",
        "disconnect_voice" => "Disconnect from voice",
        "set_server_mute" if details["muted"] == true => "Server mute",
        "set_server_mute" => "Release server mute",
        "set_server_deafen" if details["deafened"] == true => "Server deafen",
        "set_server_deafen" => "Release server deafen",
        "timeout_member" => {
            return details["duration_seconds"].as_u64().map_or_else(
                || crate::i18n::t("Remove timeout").to_owned(),
                |seconds| {
                    thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "Timeout: {} seconds",
                        seconds
                    )
                },
            );
        }
        "move_voice" => {
            return thiscord_frontend::localized_format!(
                crate::i18n::language(),
                "Move: {} → {}",
                details["source_channel_id"].as_str().unwrap_or("?"),
                details["channel_id"].as_str().unwrap_or("?")
            );
        }
        _ => return entry.action.clone(),
    };
    crate::i18n::t(label).to_owned()
}

#[derive(Clone, Copy)]
struct Panel {
    ui: Ui,
    guild: GuildId,
    state: RwSignal<Option<ModerationState>>,
    message: RwSignal<String>,
    reason: RwSignal<String>,
    audit: RwSignal<Option<thiscord_shared::pagination::Page<AuditEntry>>>,
}
impl Panel {
    async fn load(self) {
        let result = super::servers::request(
            self.ui,
            PermissionRequest::InspectModeration {
                guild_id: self.guild,
            },
        )
        .await;
        if self.state.try_get_untracked().is_none() {
            return;
        }
        match result {
            Ok(PermissionResponse::Moderation { state }) => self.state.set(Some(state)),
            Ok(_) => self.message.set("Unexpected server response".into()),
            Err(error) => {
                self.state.set(None);
                self.message.set(error);
            }
        }
    }
    async fn audit(self, after: Option<thiscord_shared::pagination::PageCursor>) {
        let result = super::servers::request(
            self.ui,
            PermissionRequest::AuditLog {
                guild_id: self.guild,
                page: thiscord_shared::pagination::PageRequest {
                    after,
                    ..Default::default()
                },
            },
        )
        .await;
        if self.audit.is_disposed() {
            return;
        }
        match result {
            Ok(PermissionResponse::AuditLog { page }) => self.audit.set(Some(page)),
            Err(e) => {
                self.audit.set(None);
                self.message.set(e);
            }
            _ => {}
        }
    }
    fn change(self, change: GuildChange) {
        if self.ui.busy.get_untracked() {
            return;
        }
        let Some(state) = self.state.get_untracked() else {
            return;
        };
        self.ui.busy.set(true);
        self.message.set(String::new());
        leptos::task::spawn_local(async move {
            let result = super::servers::request(
                self.ui,
                PermissionRequest::Moderate {
                    reason: self.reason.get_untracked(),
                    guild_id: self.guild,
                    revision: state.guild.revision,
                    change,
                },
            )
            .await;
            if self.state.try_get_untracked().is_some() {
                self.message.set(match result {
                    Ok(_) => "Action completed.".into(),
                    Err(error) => thiscord_frontend::localized_format!(crate::i18n::language(), "{error}. The member list has been refreshed; review it before trying again.", error = crate::i18n::message(error)),
                });
                self.load().await;
                self.audit(None).await;
                super::servers::refresh(self.ui).await;
            }
            self.ui.busy.set(false);
        });
    }
}

#[component]
pub(super) fn ModerationPanel(ui: Ui) -> impl IntoView {
    let home = ui.server.get_untracked();
    view! { {home.map(|home|view! { <ServerModeration ui=ui guild=home.guild.id/> })} }
}

#[component]
fn ServerModeration(ui: Ui, guild: GuildId) -> impl IntoView {
    let panel = Panel {
        ui,
        guild,
        state: RwSignal::new(None),
        message: RwSignal::new(String::new()),
        reason: RwSignal::new(String::new()),
        audit: RwSignal::new(None),
    };
    let selected = RwSignal::new(ui.moderation_target.get_untracked());
    let duration = RwSignal::new(600u32);
    let destination = RwSignal::new(None::<thiscord_shared::ChannelId>);
    let source = move || {
        ui.voice_roster
            .get()
            .into_iter()
            .find(|c| {
                c.members
                    .iter()
                    .any(|m| Some(m.account_id) == selected.get())
            })
            .map(|c| c.channel_id)
    };
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let pending = RwSignal::new(None::<(GuildChange, String, String)>);
    let member = move || {
        panel.state.get().and_then(|s| {
            s.members
                .into_iter()
                .find(|m| Some(m.account_id) == selected.get())
        })
    };
    let allowed = move |permission| member().is_some_and(|m| m.actions.contains(&permission));
    let confirm = move |change: GuildChange, title: String, detail: String| {
        pending.set(Some((change, title, detail)));
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    };
    leptos::task::spawn_local(async move {
        panel.load().await;
        panel.audit(None).await;
    });
    view! {
        <section class="space-y-6">
            <p class="settings-hint">{move || crate::i18n::t("Manage members below your highest role. The owner and members at or above your role are protected.")}</p>
            <p role="status" class="text-sm text-amber-200">{move ||crate::i18n::message(panel.message.get())}</p>
            <button class="icon-button" disabled=move ||ui.busy.get() on:click=move |_|{
                ui.busy.set(true);leptos::task::spawn_local(async move {panel.load().await;ui.busy.set(false);});
            } aria-label=move || crate::i18n::t("Refresh members") data-tip=move || crate::i18n::t("Refresh members")><super::icons::Icon kind=super::icons::Glyph::Refresh/></button>
            <Show when=move ||panel.state.get().is_some()>
                <fieldset class="settings-card disabled:opacity-50" disabled=move ||ui.busy.get()>
                    <label class="settings-field"><span>{move || crate::i18n::t("Reason (optional, up to 500 characters)")}</span><input maxlength="500" prop:value=move ||panel.reason.get() on:input=move |e|panel.reason.set(event_target_value(&e))/></label>
                    <label class="block space-y-2"><span class="font-semibold">{move || crate::i18n::t("Member")}</span>
                        <select class="w-full rounded-lg bg-slate-900 p-3" prop:value=move ||selected.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |ev|selected.set(event_target_value(&ev).parse::<AccountId>().ok())>
                            <option value="">{move || crate::i18n::t("Choose a member")}</option>
                            {move ||panel.state.get().map(|s|s.members.into_iter().map(|m|view! {<option value=m.account_id.to_string()>{m.display_name().to_owned()}</option>}).collect_view())}
                        </select>
                    </label>
                    <Show when=move ||member().is_some()>
                        <p class="text-sm text-white/60">{move ||member().map(|m|match m.timeout_until {Some(until)=>thiscord_frontend::localized_format!(crate::i18n::language(), "Timeout expiry: {}. Expired timeouts no longer restrict access.",crate::i18n::datetime(until)),None=>crate::i18n::t("No timeout set.").into()})}</p>
                        <Show when=move ||member().is_some_and(|m|m.actions.is_empty())><p class="text-sm text-white/60">{move || crate::i18n::t("Your permissions or role hierarchy do not allow actions on this member.")}</p></Show>
                        <Show when=move ||allowed(Permission::MuteMembers)>
                            <button class="settings-secondary" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::SetServerMute {account_id:m.account_id,muted:!m.server_muted},if m.server_muted{"Release server mute?"}else{"Server mute this member?"}.into(),"Server mute blocks publication in this server. Releasing it preserves their own mute setting and does not restart sharing.".into());}>{move ||crate::i18n::t(if member().is_some_and(|m|m.server_muted){"Release server mute"}else{"Server mute"})}</button>
                        </Show>
                        <Show when=move ||allowed(Permission::DeafenMembers)>
                            <button class="settings-secondary" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::SetServerDeafen {account_id:m.account_id,deafened:!m.server_deafened},if m.server_deafened{"Release server deafen?"}else{"Server deafen this member?"}.into(),"Server deafen blocks publication and reception in this server. Releasing it preserves their own mute/deafen settings.".into());}>{move ||crate::i18n::t(if member().is_some_and(|m|m.server_deafened){"Release server deafen"}else{"Server deafen"})}</button>
                        </Show>
                        <Show when=move ||allowed(Permission::MoveMembers)>
                            <label class="settings-field"><span>{move || crate::i18n::t("Move to voice channel")}</span><select prop:value=move ||destination.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |e|destination.set(event_target_value(&e).parse().ok())><option value="">{move || crate::i18n::t("Choose destination")}</option>{move ||ui.server.get().map(|s|s.channels.into_iter().filter(|c|c.kind==ChannelKind::Voice && Some(c.id)!=source()).map(|c|view!{<option value=c.id.to_string()>{c.name}</option>}).collect_view())}</select></label>
                            <button class="settings-secondary" disabled=move ||source().is_none() || destination.get().is_none() on:click=move |_|if let (Some(m),Some(source_channel_id),Some(channel_id))=(member(),source(),destination.get_untracked()){confirm(GuildChange::MoveVoice {account_id:m.account_id,source_channel_id,channel_id},"Move this member?".into(),"Ends the old transport and reconnects the desktop client after fresh destination admission. Self mute/deafen are preserved; screen and camera sharing stop. Both you and the member must have access to both channels.".into());}>{move || crate::i18n::t("Move member")}</button>
                        </Show>
                        <Show when=move ||allowed(Permission::MoveMembers)>
                            <button class="rounded-lg border border-white/20 px-4 py-2 hover:bg-white/10" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::DisconnectVoice{account_id:m.account_id},thiscord_frontend::localized_format!(crate::i18n::language(), "Disconnect {} from voice?",m.display_name().to_owned()),"Ends their current voice and screen-sharing connections in this server. They can join again manually.".into());}>{move || crate::i18n::t("Disconnect from voice")}</button>
                        </Show>
                        <Show when=move ||allowed(Permission::ModerateMembers)>
                            <div class="space-y-3 rounded-xl border border-white/10 p-4"><h3 class="font-semibold">{move || crate::i18n::t("Timeout")}</h3><p class="text-sm text-white/60">{move || crate::i18n::t("Temporarily prevents messaging, moderation actions, and joining voice or sharing a screen. The member can still read channels they have access to.")}</p>
                                <label class="flex flex-wrap items-center gap-3"><span>{move || crate::i18n::t("Duration")}</span><select class="rounded bg-slate-900 p-2" prop:value=move ||duration.get().to_string() on:change=move |ev|if let Ok(value)=event_target_value(&ev).parse(){duration.set(value);}>
                                    <option value="60">{move || crate::i18n::t("1 minute")}</option><option value="600">{move || crate::i18n::t("10 minutes")}</option><option value="3600">{move || crate::i18n::t("1 hour")}</option><option value="86400">{move || crate::i18n::t("1 day")}</option><option value="604800">{move || crate::i18n::t("1 week")}</option><option value="2419200">{move || crate::i18n::t("28 days")}</option>
                                </select></label>
                                <div class="flex flex-wrap gap-3"><button class="rounded bg-amber-500/20 px-4 py-2 text-amber-200" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::TimeoutMember{account_id:m.account_id,duration_seconds:Some(duration.get_untracked())},thiscord_frontend::localized_format!(crate::i18n::language(), "Timeout {}?",m.display_name().to_owned()),thiscord_frontend::localized_format!(crate::i18n::language(), "Restrict communication for {} minutes. Current voice and screen sharing will stop.",duration.get_untracked()/60));}>{move || crate::i18n::t("Apply timeout")}</button>
                                <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-40" disabled=move ||member().is_none_or(|m|m.timeout_until.is_none()) on:click=move |_|if let Some(m)=member(){panel.change(GuildChange::TimeoutMember{account_id:m.account_id,duration_seconds:None});}>{move || crate::i18n::t("Remove timeout")}</button></div>
                            </div>
                        </Show>
                        <div class="flex flex-wrap gap-3">
                            <Show when=move ||allowed(Permission::KickMembers)><button class="rounded-lg border border-red-400/40 px-4 py-2 text-red-300" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::RemoveMember{account_id:m.account_id},thiscord_frontend::localized_format!(crate::i18n::language(), "Kick {} from this server?",m.display_name().to_owned()),"Removes their membership and roles, and disconnects voice. They can rejoin using the server's join credentials.".into());}>{move || crate::i18n::t("Kick from server")}</button></Show>
                            <Show when=move ||allowed(Permission::BanMembers)><button class="rounded-lg bg-red-500/20 px-4 py-2 text-red-300" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::BanMember{account_id:m.account_id},thiscord_frontend::localized_format!(crate::i18n::language(), "Ban {} from this server?",m.display_name().to_owned()),"Removes their membership and roles, disconnects voice, and prevents rejoining until unbanned. Existing messages are retained.".into());}>{move || crate::i18n::t("Ban from server")}</button></Show>
                        </div>
                    </Show>
                    <Show when=move ||panel.state.get().is_some_and(|s|s.can_unban)>
                        <section class="space-y-3 border-t border-white/10 pt-5"><h3 class="text-lg font-semibold">{move || crate::i18n::t("Banned members")}</h3>
                            <Show when=move ||panel.state.get().is_some_and(|s|s.bans.is_empty())><p class="text-sm text-white/60">{move || crate::i18n::t("No banned members.")}</p></Show>
                            <ul class="space-y-3">{move ||panel.state.get().map(|s|s.bans.into_iter().map(move |b|view! {<li class="flex items-center justify-between gap-3"><span>{b.display_name().to_owned()}</span><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|confirm(GuildChange::UnbanMember{account_id:b.account_id},thiscord_frontend::localized_format!(crate::i18n::language(), "Unban {}?",b.display_name().to_owned()),"Allows them to join again. Their previous membership and roles will not be restored.".into())>{move || crate::i18n::t("Unban")}</button></li>}).collect_view())}</ul>
                        </section>
                    </Show>
                </fieldset>
            </Show>
        </section>
        <section class="settings-card"><h3>{move || crate::i18n::t("Moderation audit log")}</h3><p class="settings-hint">{move || crate::i18n::t("Last 1000 actions retained, displayed oldest first. Reasons are visible to server moderators.")}</p><button class="settings-secondary" on:click=move |_|leptos::task::spawn_local(async move {panel.audit(None).await;})>{move || crate::i18n::t("Refresh log")}</button>
            <ul class="space-y-3">{move ||panel.audit.get().map(|page|page.items.into_iter().map(|entry|view!{<li class="border-b border-white/10 py-2 text-sm"><p>{format!("{} · {}",crate::i18n::datetime(entry.created_at),audit_description(&entry))}</p><p>{thiscord_frontend::localized_format!(crate::i18n::language(), "Actor: {} · Member: {}",entry.actor.map(|id|id.to_string()).unwrap_or_else(||crate::i18n::t("Deleted account").into()),entry.target.map(|id|id.to_string()).unwrap_or_else(||crate::i18n::t("Deleted account").into()))}</p><p class="break-words">{entry.reason}</p></li>}).collect_view())}</ul>
            <button class="settings-secondary" disabled=move ||panel.audit.get().is_none_or(|p|p.next_cursor.is_none()) on:click=move |_|if let Some(after)=panel.audit.get_untracked().and_then(|p|p.next_cursor){leptos::task::spawn_local(async move {panel.audit(Some(after)).await;});}>{move || crate::i18n::t("Next page")}</button>
        </section>
        <dialog node_ref=dialog on:click=move |e|super::dialog::dismiss_outside(e,dialog,ui.busy.get_untracked()) aria-labelledby="moderation-confirm-title" class="m-auto w-[calc(100%-2rem)] max-w-md space-y-5 rounded-2xl border border-white/15 bg-surface p-6 text-white shadow-2xl backdrop:bg-black/70" on:close=move |_|pending.set(None)>
            <h2 id="moderation-confirm-title" class="text-xl font-semibold">{move ||pending.get().map(|(_,title,_)|crate::i18n::message(title))}</h2>
            <p class="text-sm text-white/70">{move ||pending.get().map(|(_,_,detail)|crate::i18n::message(detail))}</p>
            <div class="flex justify-end gap-3"><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>{move || crate::i18n::t("Cancel")}</button><button class="rounded bg-red-500/20 px-4 py-2 text-red-200" on:click=move |_|{
                if let Some((change,_,_))=pending.get_untracked(){panel.change(change);}
                if let Some(dialog)=dialog.get(){dialog.close();}
            }>{move || crate::i18n::t("Confirm")}</button></div>
        </dialog>
    }
}
