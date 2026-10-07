use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{AccountId, GuildId, permissions::*};

#[derive(Clone, Copy)]
struct Panel {
    ui: Ui,
    guild: GuildId,
    state: RwSignal<Option<ModerationState>>,
    message: RwSignal<String>,
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
                PermissionRequest::Change {
                    guild_id: self.guild,
                    revision: state.guild.revision,
                    change,
                },
            )
            .await;
            if self.state.try_get_untracked().is_some() {
                self.message.set(match result {
                    Ok(_) => "Action completed.".into(),
                    Err(error) => format!("{error}. The member list has been refreshed; review it before trying again."),
                });
                self.load().await;
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
    };
    let selected = RwSignal::new(ui.moderation_target.get_untracked());
    let duration = RwSignal::new(600u32);
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
    });
    view! {
        <section class="space-y-6">
            <header class="flex flex-wrap items-center justify-between gap-3">
                <div><p class="text-sm text-white/60">{move ||panel.state.get().map(|s|s.guild.name)}</p><h2 class="text-2xl font-semibold">"Server moderation"</h2></div>
                <button class="rounded bg-white/10 px-4 py-2" on:click=move |_|ui.page.set("servers")>"Back to server"</button>
            </header>
            <p class="text-sm text-white/60">"Actions apply only to this server. You can moderate members below your highest role. The owner and members at or above your role are protected."</p>
            <p role="status" class="text-sm text-amber-200">{move ||panel.message.get()}</p>
            <button class="text-sm text-brand underline disabled:opacity-50" disabled=move ||ui.busy.get() on:click=move |_|{
                ui.busy.set(true);leptos::task::spawn_local(async move {panel.load().await;ui.busy.set(false);});
            }>"Refresh members"</button>
            <Show when=move ||panel.state.get().is_some()>
                <fieldset class="space-y-5 disabled:opacity-50" disabled=move ||ui.busy.get()>
                    <label class="block space-y-2"><span class="font-semibold">"Member"</span>
                        <select class="w-full rounded-lg bg-slate-900 p-3" prop:value=move ||selected.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |ev|selected.set(event_target_value(&ev).parse::<AccountId>().ok())>
                            <option value="">"Choose a member"</option>
                            {move ||panel.state.get().map(|s|s.members.into_iter().map(|m|view! {<option value=m.account_id.to_string()>{m.username}</option>}).collect_view())}
                        </select>
                    </label>
                    <Show when=move ||member().is_some()>
                        <p class="text-sm text-white/60">{move ||member().map(|m|match m.timeout_until {Some(until)=>format!("Timeout expiry: {} (UTC). Expired timeouts no longer restrict access.",until.format("%Y-%m-%d %H:%M:%S")),None=>"No timeout set.".into()})}</p>
                        <Show when=move ||member().is_some_and(|m|m.actions.is_empty())><p class="text-sm text-white/60">"Your permissions or role hierarchy do not allow actions on this member."</p></Show>
                        <Show when=move ||allowed(Permission::MoveMembers)>
                            <button class="rounded-lg border border-white/20 px-4 py-2 hover:bg-white/10" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::DisconnectVoice{account_id:m.account_id},format!("Disconnect {} from voice?",m.username),"Ends their current voice and screen-sharing connections in this server. They can join again manually.".into());}>"Disconnect from voice"</button>
                        </Show>
                        <Show when=move ||allowed(Permission::ModerateMembers)>
                            <div class="space-y-3 rounded-xl border border-white/10 p-4"><h3 class="font-semibold">"Timeout"</h3><p class="text-sm text-white/60">"Temporarily prevents messaging, moderation actions, and joining voice or sharing a screen. The member can still read channels they have access to."</p>
                                <label class="flex flex-wrap items-center gap-3"><span>"Duration"</span><select class="rounded bg-slate-900 p-2" prop:value=move ||duration.get().to_string() on:change=move |ev|if let Ok(value)=event_target_value(&ev).parse(){duration.set(value);}>
                                    <option value="60">"1 minute"</option><option value="600">"10 minutes"</option><option value="3600">"1 hour"</option><option value="86400">"1 day"</option><option value="604800">"1 week"</option><option value="2419200">"28 days"</option>
                                </select></label>
                                <div class="flex flex-wrap gap-3"><button class="rounded bg-amber-500/20 px-4 py-2 text-amber-200" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::TimeoutMember{account_id:m.account_id,duration_seconds:Some(duration.get_untracked())},format!("Timeout {}?",m.username),format!("Restrict communication for {} minutes. Current voice and screen sharing will stop.",duration.get_untracked()/60));}>"Apply timeout"</button>
                                <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-40" disabled=move ||member().is_none_or(|m|m.timeout_until.is_none()) on:click=move |_|if let Some(m)=member(){panel.change(GuildChange::TimeoutMember{account_id:m.account_id,duration_seconds:None});}>"Remove timeout"</button></div>
                            </div>
                        </Show>
                        <div class="flex flex-wrap gap-3">
                            <Show when=move ||allowed(Permission::KickMembers)><button class="rounded-lg border border-red-400/40 px-4 py-2 text-red-300" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::RemoveMember{account_id:m.account_id},format!("Kick {} from this server?",m.username),"Removes their membership and roles, and disconnects voice. They can rejoin using the server's join credentials.".into());}>"Kick from server"</button></Show>
                            <Show when=move ||allowed(Permission::BanMembers)><button class="rounded-lg bg-red-500/20 px-4 py-2 text-red-300" on:click=move |_|if let Some(m)=member(){confirm(GuildChange::BanMember{account_id:m.account_id},format!("Ban {} from this server?",m.username),"Removes their membership and roles, disconnects voice, and prevents rejoining until unbanned. Existing messages are retained.".into());}>"Ban from server"</button></Show>
                        </div>
                    </Show>
                    <Show when=move ||panel.state.get().is_some_and(|s|s.can_unban)>
                        <section class="space-y-3 border-t border-white/10 pt-5"><h3 class="text-lg font-semibold">"Banned members"</h3>
                            <Show when=move ||panel.state.get().is_some_and(|s|s.bans.is_empty())><p class="text-sm text-white/60">"No banned members."</p></Show>
                            <ul class="space-y-3">{move ||panel.state.get().map(|s|s.bans.into_iter().map(move |b|view! {<li class="flex items-center justify-between gap-3"><span>{b.username.clone()}</span><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|confirm(GuildChange::UnbanMember{account_id:b.account_id},format!("Unban {}?",b.username),"Allows them to join again. Their previous membership and roles will not be restored.".into())>"Unban"</button></li>}).collect_view())}</ul>
                        </section>
                    </Show>
                </fieldset>
            </Show>
        </section>
        <dialog node_ref=dialog aria-labelledby="moderation-confirm-title" class="m-auto w-[calc(100%-2rem)] max-w-md space-y-5 rounded-2xl border border-white/15 bg-surface p-6 text-white shadow-2xl backdrop:bg-black/70" on:close=move |_|pending.set(None)>
            <h2 id="moderation-confirm-title" class="text-xl font-semibold">{move ||pending.get().map(|(_,title,_)|title)}</h2>
            <p class="text-sm text-white/70">{move ||pending.get().map(|(_,_,detail)|detail)}</p>
            <div class="flex justify-end gap-3"><button class="rounded bg-white/10 px-4 py-2" on:click=move |_|if let Some(dialog)=dialog.get(){dialog.close();}>"Cancel"</button><button class="rounded bg-red-500/20 px-4 py-2 text-red-200" on:click=move |_|{
                if let Some((change,_,_))=pending.get_untracked(){panel.change(change);}
                if let Some(dialog)=dialog.get(){dialog.close();}
            }>"Confirm"</button></div>
        </dialog>
    }
}
