use super::{Field, Ui};
use leptos::prelude::*;
use thiscord_shared::{AccountId, permissions::*};

#[derive(Clone, Copy)]
struct Editor {
    ui: Ui,
    guilds: RwSignal<Vec<Guild>>,
    state: RwSignal<Option<GuildState>>,
    access: RwSignal<Option<InstanceAccess>>,
    preview: RwSignal<String>,
    selected_role: RwSignal<String>,
}
impl Editor {
    fn request(self, command: PermissionRequest) {
        if self.ui.busy.get_untracked() {
            return;
        }
        self.ui.busy.set(true);
        let creating_role = matches!(
            &command,
            PermissionRequest::Change {
                change: GuildChange::CreateRole { .. },
                ..
            }
        );
        leptos::task::spawn_local(async move {
            let token = self.ui.token.get_untracked();
            let result = crate::account_client::api_request::<_, PermissionResponse>(
                PERMISSIONS_PATH,
                &command,
                token.as_deref(),
            )
            .await;
            match result {
                Ok(response) => {
                    self.ui.status.set(String::new());
                    match response {
                        PermissionResponse::Joined { .. } | PermissionResponse::Home { .. } => {}
                        PermissionResponse::Instance { access } => self.access.set(Some(access)),
                        PermissionResponse::Guilds { guilds } => self.guilds.set(guilds),
                        PermissionResponse::State { state } => {
                            if creating_role {
                                let previous = self.state.get_untracked();
                                if let Some(role) = state.roles.iter().find(|r| {
                                    !r.everyone
                                        && previous.as_ref().is_some_and(|s| {
                                            !s.roles.iter().any(|old| old.id == r.id)
                                        })
                                }) {
                                    self.selected_role.set(role.id.to_string());
                                }
                            }
                            let guild = state.guild.clone();
                            self.guilds.update(|guilds| {
                                guilds.retain(|g| g.id != guild.id);
                                guilds.push(guild);
                            });
                            self.preview.set(String::new());
                            self.state.set(Some(state));
                        }
                        PermissionResponse::Effective { permissions, .. } => {
                            self.preview.set(format!("{permissions:?}"))
                        }
                        PermissionResponse::Done => {
                            self.state.set(None);
                            self.preview.set(String::new());
                        }
                    }
                    super::servers::refresh(self.ui).await;
                    self.guilds.set(self.ui.guilds.get_untracked());
                }
                Err(error) => self.ui.status.set(error),
            }
            self.ui.busy.set(false);
        });
    }
    fn change(self, change: GuildChange) {
        if let Some(state) = self.state.get_untracked() {
            self.request(PermissionRequest::Change {
                guild_id: state.guild.id,
                revision: state.guild.revision,
                change,
            });
        }
    }
}

#[component]
fn PermissionChecks(value: RwSignal<Permissions>, label: &'static str) -> impl IntoView {
    view! { <fieldset class="space-y-2"><legend class="font-semibold">{label}</legend>
    <div class="grid gap-2 sm:grid-cols-2">{Permission::ALL.into_iter().map(move |p|view! {
        <label class="flex items-center gap-2 text-sm"><input type="checkbox" prop:checked=move ||value.get().contains(&p) on:change=move |ev|value.update(|v| {if event_target_checked(&ev) {v.insert(p);} else {v.remove(&p);}})/>{format!("{p:?}")}</label>
    }).collect_view()}</div></fieldset> }
}

#[component]
pub(super) fn PermissionEditor(ui: Ui) -> impl IntoView {
    let editor = Editor {
        ui,
        guilds: RwSignal::new(ui.guilds.get_untracked()),
        state: RwSignal::new(None),
        access: RwSignal::new(None),
        preview: RwSignal::new(String::new()),
        selected_role: RwSignal::new(String::new()),
    };
    let member_name = RwSignal::new(String::new());
    let member_id = RwSignal::new(String::new());
    let role_id = editor.selected_role;
    let role_name = RwSignal::new(String::new());
    let position = RwSignal::new("1".to_string());
    let grants = RwSignal::new(Permissions::new());
    let channel_id = RwSignal::new(String::new());
    let channel_name = RwSignal::new(String::new());
    let voice = RwSignal::new(false);
    let member_target = RwSignal::new(false);
    let allow = RwSignal::new(Permissions::new());
    let deny = RwSignal::new(Permissions::new());
    let confirmation = RwSignal::new(String::new());
    let delete_confirmation = RwSignal::new(String::new());
    let instance_confirmation = RwSignal::new(String::new());
    let instance_target = RwSignal::new(String::new());
    let target = move || -> Option<OverrideTarget> {
        if member_target.get_untracked() {
            member_id
                .get_untracked()
                .parse()
                .ok()
                .map(OverrideTarget::Member)
        } else {
            role_id
                .get_untracked()
                .parse()
                .ok()
                .map(OverrideTarget::Role)
        }
    };
    let confirmed = move || {
        editor
            .state
            .get()
            .is_some_and(|s| confirmation.get() == s.guild.name)
    };
    let load_role = move |value: String| {
        role_id.set(value.clone());
        if let Some(r) = editor
            .state
            .get_untracked()
            .and_then(|s| s.roles.into_iter().find(|r| r.id.to_string() == value))
        {
            role_name.set(r.name);
            position.set(r.position.to_string());
            grants.set(r.permissions);
        } else {
            role_name.set(String::new());
            position.set("1".into());
            grants.set(Permissions::new());
        }
    };
    let assign = move |assigned| {
        let Ok(account_id) = member_id.get_untracked().parse() else {
            ui.status.set("Select a member first".into());
            return;
        };
        let Ok(role_id) = role_id.get_untracked().parse() else {
            ui.status
                .set("Save a new role or select an existing role first".into());
            return;
        };
        if let Some(state) = editor.state.get_untracked() {
            if state.roles.iter().any(|r| r.id == role_id && r.everyone) {
                ui.status.set(
                    "Everyone already applies to every member and cannot be assigned or removed"
                        .into(),
                );
                return;
            }
            if account_id == state.guild.owner {
                ui.status
                    .set("The server owner already has full administrator permissions".into());
                return;
            }
        }
        editor.change(GuildChange::AssignRole {
            account_id,
            role_id,
            assigned,
        });
    };
    if let Some(home) = ui.server.get_untracked().filter(|s| s.can_manage_roles) {
        editor.request(PermissionRequest::Inspect {
            guild_id: home.guild.id,
        });
    } else {
        editor.request(PermissionRequest::ListGuilds {});
    }
    view! {
        <div class="space-y-6">
            <h2 class="text-2xl font-semibold">"Guilds & roles"</h2>
            <p class="text-sm text-white/60">"Instance roles and guild roles are independent. Refresh after a conflict. Ownership changes require recent reauthentication."</p>
            <fieldset class="space-y-6 disabled:opacity-60" disabled=move ||ui.busy.get()>
                <div class="flex flex-wrap gap-4"><button class="underline" on:click=move |_|editor.request(PermissionRequest::ListGuilds {})>"Refresh guilds"</button><button class="underline" on:click=move |_|editor.request(PermissionRequest::Instance {})>"Instance access"</button></div>
                <Show when=move ||editor.access.get().is_some()>
                    <p>{move ||editor.access.get().map(|a|format!("Instance role: {:?}",a.role))}</p>
                    <Show when=move ||editor.access.get().is_some_and(|a|a.role==InstanceRole::Owner)>
                        <Field label="Verified account ID for instance administration" value=instance_target/>
                        <div class="flex flex-wrap gap-4">
                            <button class="underline" on:click=move |_|if let Ok(id)=instance_target.get_untracked().parse(){editor.request(PermissionRequest::SetInstanceAdmin{account_id:id,admin:true});}>"Make instance admin"</button>
                            <button class="underline" on:click=move |_|if let Ok(id)=instance_target.get_untracked().parse(){editor.request(PermissionRequest::SetInstanceAdmin{account_id:id,admin:false});}>"Remove instance admin"</button>
                            <button class="underline" disabled=move ||instance_confirmation.get()!="TRANSFER INSTANCE" on:click=move |_|if instance_confirmation.get_untracked()=="TRANSFER INSTANCE" && let Ok(id)=instance_target.get_untracked().parse(){editor.request(PermissionRequest::TransferInstance{account_id:id});}>"Transfer instance ownership"</button>
                        </div>
                        <p class="text-sm text-white/60">"To transfer the instance, type TRANSFER INSTANCE here, then click Transfer instance ownership. You will become a regular user."</p>
                        <Field label="Confirm instance transfer" value=instance_confirmation/>
                    </Show>
                </Show>
                <ul class="space-y-2">{move ||editor.guilds.get().into_iter().map(move |g| {let id=g.id;let revision=g.revision;view! {
                    <li class="flex flex-wrap justify-between gap-3"><span>{g.name}</span><div class="flex gap-4">
                        <button class="underline" on:click=move |_| {role_id.set(String::new());member_id.set(String::new());channel_id.set(String::new());confirmation.set(String::new());delete_confirmation.set(String::new());editor.request(PermissionRequest::Inspect{guild_id:id});}>"Server settings"</button>
                        <button class="underline" on:click=move |_|if let Some(a)=ui.account.get_untracked(){editor.request(PermissionRequest::Preview{guild_id:id,account_id:a.id,channel_id:None});}>"My permissions"</button>
                        <Show when=move ||ui.account.get().is_some_and(|a|a.id==g.owner) fallback=move ||view!{
                            <button class="underline" on:click=move |_|if window().confirm_with_message("Leave this server? You will lose access to its channels.").unwrap_or(false){editor.request(PermissionRequest::Change{guild_id:id,revision,change:GuildChange::Leave {}});}>"Leave server"</button>
                        }><span class="cursor-not-allowed" tabindex="0" title="Owner: transfer ownership or delete the server to leave."><button type="button" class="pointer-events-none text-white/30 underline" disabled aria-label="Leave server unavailable: transfer ownership or delete the server first">"Leave server"</button></span></Show>
                    </div></li>
                }}).collect_view()}</ul>
                <Show when=move ||editor.state.get().is_some()>
                    <h3 class="text-xl font-semibold">{move ||editor.state.get().map(|s|s.guild.name)}</h3>
                    <Show when=move ||editor.state.get().is_some_and(|s|ui.account.get().is_some_and(|a|a.id==s.guild.owner))>
                        <p class="text-sm text-brand">"You are the server owner. You have full administrator permissions and can create and assign roles to other members."</p>
                    </Show>
                    <button class="underline" on:click=move |_|if let Some(s)=editor.state.get_untracked(){editor.request(PermissionRequest::Inspect{guild_id:s.guild.id});}>"Refresh selected guild"</button>
                    <Field label="Add verified member by username" value=member_name/>
                    <button class="underline" on:click=move |_|editor.change(GuildChange::AddMember{username:member_name.get_untracked()})>"Add member"</button>
                    <label class="block space-y-2"><span>"Member"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move ||member_id.get() on:change=move |ev|member_id.set(event_target_value(&ev))>
                        <option value="">"Select member"</option>{move ||editor.state.get().map(|s|s.members.into_iter().map(|m|view!{<option value=m.account_id.to_string()>{if m.account_id==s.guild.owner {format!("{} (Owner)",m.username)} else {m.username}}</option>}).collect_view())}
                    </select></label>
                    <p class="break-all text-sm text-white/60">{move ||editor.state.get().and_then(|s|s.members.iter().find(|m|m.account_id.to_string()==member_id.get()).map(|m|format!("Account: {} · {}Roles: {}",m.account_id,if m.account_id==s.guild.owner {"Owner (full administrator access) · "} else {""},s.roles.iter().filter(|r|r.everyone || m.roles.contains(&r.id)).map(|r|r.name.clone()).collect::<Vec<_>>().join(", "))))}</p>
                    <label class="block space-y-2"><span>"Role"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move ||role_id.get() on:change=move |ev|load_role(event_target_value(&ev))>
                        <option value="">"New role"</option>{move ||editor.state.get().map(|s|s.roles.into_iter().map(|r|view!{<option value=r.id.to_string()>{format!("{} (rank {})",r.name,r.position)}</option>}).collect_view())}
                    </select></label>
                    <Field label="Role name" value=role_name/><Field label="Rank (1–10000; Everyone stays 0)" value=position kind="number"/>
                    <PermissionChecks value=grants label="Role permissions"/>
                    <div class="flex flex-wrap gap-4">
                        <button class="rounded-md bg-brand px-4 py-2" on:click=move |_| {
                            let Ok(p)=position.get_untracked().parse() else {ui.status.set("Enter a valid rank".into());return;};
                            let change=if let Ok(id)=role_id.get_untracked().parse() {GuildChange::EditRole{role_id:id,name:role_name.get_untracked(),position:p,permissions:grants.get_untracked()}}
                            else {GuildChange::CreateRole{name:role_name.get_untracked(),position:p,permissions:grants.get_untracked()}};
                            editor.change(change);
                        }>"Save role"</button>
                        <button class="underline" on:click=move |_|assign(true)>"Assign role"</button>
                        <button class="underline" on:click=move |_|assign(false)>"Unassign role"</button>
                    </div>
                    <Field label="New channel name" value=channel_name/>
                    <label class="flex gap-2"><input type="checkbox" on:change=move |ev|voice.set(event_target_checked(&ev))/>"Voice channel"</label>
                    <button class="underline" on:click=move |_|editor.change(GuildChange::CreateChannel{name:channel_name.get_untracked(),kind:if voice.get_untracked(){ChannelKind::Voice}else{ChannelKind::Text}})>"Create channel"</button>
                    <label class="block space-y-2"><span>"Channel"</span><select class="w-full rounded bg-slate-900 p-2" prop:value=move ||channel_id.get() on:change=move |ev|channel_id.set(event_target_value(&ev))>
                        <option value="">"Guild base permissions"</option>{move ||editor.state.get().map(|s|s.channels.into_iter().map(|ch|view!{<option value=ch.id.to_string()>{ch.name}</option>}).collect_view())}
                    </select></label>
                    <h3 class="font-semibold">"Channel overrides"</h3>
                    <label class="flex gap-2"><input type="checkbox" on:change=move |ev|member_target.set(event_target_checked(&ev))/>"Override selected member (otherwise selected role)"</label>
                    <button class="underline" on:click=move |_| {
                        let old=editor.state.get_untracked().and_then(|s|s.overrides.into_iter().find(|o|o.channel_id.to_string()==channel_id.get_untracked() && Some(o.target)==target()));
                        allow.set(old.as_ref().map(|o|o.allow.clone()).unwrap_or_default());deny.set(old.map(|o|o.deny).unwrap_or_default());
                    }>"Load saved override"</button>
                    <PermissionChecks value=allow label="Allow"/><PermissionChecks value=deny label="Deny"/>
                    <div class="flex flex-wrap gap-4">
                        <button class="underline" on:click=move |_|if let (Ok(channel_id),Some(target))=(channel_id.get_untracked().parse(),target()){editor.change(GuildChange::SetOverride{channel_id,target,allow:allow.get_untracked(),deny:deny.get_untracked()});}>"Save override"</button>
                        <button class="underline" on:click=move |_|if let (Ok(channel_id),Some(target))=(channel_id.get_untracked().parse(),target()){editor.change(GuildChange::DeleteOverride{channel_id,target});}>"Clear override"</button>
                        <button class="underline" on:click=move |_|if let (Some(s),Ok(account_id))=(editor.state.get_untracked(),member_id.get_untracked().parse::<AccountId>()){editor.request(PermissionRequest::Preview{guild_id:s.guild.id,account_id,channel_id:channel_id.get_untracked().parse().ok()});}>"Preview effective permissions"</button>
                    </div>
                    <p class="text-sm text-white/60">"Type the selected server name, then click the specific action below. Select the role, channel or member to change first."</p>
                    <Field label="Server name to confirm the action" value=confirmation/>
                    <div class="flex flex-wrap gap-4">
                        <button class="text-red-300 underline" disabled=move ||!confirmed() on:click=move |_|if let Ok(id)=role_id.get_untracked().parse(){editor.change(GuildChange::DeleteRole{role_id:id});}>"Delete role"</button>
                        <button class="text-red-300 underline" disabled=move ||!confirmed() on:click=move |_|if let Ok(id)=channel_id.get_untracked().parse(){editor.change(GuildChange::DeleteChannel{channel_id:id});}>"Delete channel"</button>
                        <button class="text-red-300 underline" disabled=move ||!confirmed() on:click=move |_|if let Ok(id)=member_id.get_untracked().parse(){editor.change(GuildChange::RemoveMember{account_id:id});}>"Remove member"</button>
                        <Show when=move ||editor.state.get().is_some_and(|s|ui.account.get().is_some_and(|a|a.id==s.guild.owner))><button class="text-red-300 underline disabled:opacity-40" disabled=move ||!confirmed()||!editor.state.get().is_some_and(|s|s.members.iter().any(|m|m.account_id!=s.guild.owner&&m.account_id.to_string()==member_id.get())) on:click=move |_|if let Ok(id)=member_id.get_untracked().parse(){editor.change(GuildChange::TransferOwner{account_id:id});}>"Transfer server ownership"</button></Show>
                    </div>
                </Show>
                <p class="text-sm" role="status">{move ||editor.preview.get()}</p>
                <Show when=move ||editor.state.get().is_some_and(|s|ui.account.get().is_some_and(|a|a.id==s.guild.owner))>
                        <form class="space-y-3 rounded-lg border border-red-400/30 p-4" on:submit=move |ev: leptos::ev::SubmitEvent| {
                            ev.prevent_default();
                            if editor.state.get_untracked().is_some_and(|s|delete_confirmation.get_untracked()==s.guild.name) {
                                editor.change(GuildChange::Delete {});
                            }
                        }>
                            <h4 class="font-semibold text-red-300">"Delete server"</h4>
                            <p class="text-sm text-white/60">"Permanently deletes this server, its channels and all messages. This cannot be undone. Type the server name below, then click Delete server."</p>
                            <p class="break-words font-semibold">{move ||editor.state.get().map(|s|s.guild.name)}</p>
                            <Field label="Server name to confirm deletion" value=delete_confirmation/>
                            <div class="flex flex-wrap items-center gap-4">
                                <button type="submit" class="rounded bg-red-600 px-4 py-2 font-semibold disabled:opacity-40" disabled=move ||!editor.state.get().is_some_and(|s|delete_confirmation.get()==s.guild.name)>"Delete server"</button>
                                <button type="button" class="text-sm underline" on:click=move |_|ui.page.set("reauthenticate")>"Reauthenticate"</button>
                            </div>
                            <p class="text-xs text-white/60">"Recent authentication is required. If your session is too old, reauthenticate, then return to Server settings."</p>
                        </form>
                </Show>
            </fieldset>
        </div>
    }
}
