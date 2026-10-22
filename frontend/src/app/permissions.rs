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
    fn allowed(self, permission: Permission) -> bool {
        self.state
            .get()
            .zip(self.ui.account.get())
            .is_some_and(|(s, a)| {
                a.id == s.guild.owner
                    || s.members
                        .iter()
                        .find(|m| m.account_id == a.id)
                        .is_some_and(|m| {
                            s.roles
                                .iter()
                                .filter(|r| r.everyone || m.roles.contains(&r.id))
                                .any(|r| {
                                    r.permissions.contains(&Permission::Administrator)
                                        || r.permissions.contains(&permission)
                                })
                        })
            })
    }
    fn request(self, command: PermissionRequest) {
        self.dispatch(command, None);
    }
    fn dispatch(self, command: PermissionRequest, on_success: Option<Callback<()>>) {
        if self.ui.busy.get_untracked() {
            return;
        }
        self.ui.busy.set(true);
        let mutation = matches!(
            &command,
            PermissionRequest::Change { .. }
                | PermissionRequest::SetInstanceAdmin { .. }
                | PermissionRequest::TransferInstance { .. }
        );
        let creating_role = matches!(
            &command,
            PermissionRequest::Change {
                change: GuildChange::CreateRole { .. },
                ..
            }
        );
        leptos::task::spawn_local(async move {
            let selection = self.ui.navigation.get_untracked().current();
            let token = self.ui.token.get_untracked();
            let result = crate::account_client::api_request::<_, PermissionResponse>(
                PERMISSIONS_PATH,
                &command,
                token.as_deref(),
            )
            .await;
            if self.ui.token.get_untracked() != token
                || self.state.is_disposed()
                || !super::servers::current(self.ui, selection)
            {
                self.ui.busy.set(false);
                return;
            }
            match result {
                Ok(response) => {
                    self.ui.status.set(String::new());
                    match response {
                        PermissionResponse::Moderation { .. } => {}
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
                        PermissionResponse::Effective { permissions, .. } => self.preview.set(
                            permissions
                                .into_iter()
                                .map(permission_name)
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                        PermissionResponse::Done => {
                            self.state.set(None);
                            self.preview.set(String::new());
                        }
                    }
                    if mutation {
                        super::servers::refresh(self.ui).await;
                    }
                    if self.ui.token.get_untracked() != token
                        || self.state.is_disposed()
                        || !super::servers::current(self.ui, selection)
                    {
                        self.ui.busy.set(false);
                        return;
                    }
                    if !self.state.is_disposed() {
                        self.guilds.set(self.ui.guilds.get_untracked());
                        if let Some(callback) = on_success {
                            callback.run(());
                        }
                    }
                    if mutation {
                        self.ui.status.set("Changes saved".into());
                    }
                    if super::settings::server_page(self.ui.page.get_untracked())
                        && self
                            .ui
                            .server
                            .get_untracked()
                            .is_none_or(|s| !s.can_manage_roles)
                    {
                        self.ui.page.set("servers");
                    }
                }
                Err(error) => self.ui.status.set(error),
            }
            self.ui.busy.set(false);
        });
    }
    fn change(self, change: GuildChange) {
        self.change_then(change, None);
    }
    fn change_then(self, change: GuildChange, on_success: Option<Callback<()>>) {
        if let Some(state) = self.state.get_untracked() {
            self.dispatch(
                PermissionRequest::Change {
                    guild_id: state.guild.id,
                    revision: state.guild.revision,
                    change,
                },
                on_success,
            );
        }
    }
}

fn editor(ui: Ui) -> Editor {
    Editor {
        ui,
        guilds: RwSignal::new(vec![]),
        state: RwSignal::new(None),
        access: RwSignal::new(None),
        preview: RwSignal::new(String::new()),
        selected_role: RwSignal::new(String::new()),
    }
}
fn permission_name(p: Permission) -> &'static str {
    match p {
        Permission::Administrator => crate::i18n::t("Administrator (full access)"),
        Permission::ManageGuild => crate::i18n::t("Manage server"),
        Permission::ManageChannels => crate::i18n::t("Manage channels"),
        Permission::ManageRoles => crate::i18n::t("Manage roles & permissions"),
        Permission::ManageInvites => crate::i18n::t("Add members"),
        Permission::ViewChannel => crate::i18n::t("View channel"),
        Permission::ReadHistory => crate::i18n::t("Read message history"),
        Permission::SendMessages => crate::i18n::t("Send messages"),
        Permission::EditOwnMessages => crate::i18n::t("Edit own messages"),
        Permission::DeleteOwnMessages => crate::i18n::t("Delete own messages"),
        Permission::ManageMessages => crate::i18n::t("Manage messages"),
        Permission::KickMembers => crate::i18n::t("Kick members"),
        Permission::BanMembers => crate::i18n::t("Ban members"),
        Permission::ModerateMembers => crate::i18n::t("Timeout members"),
        Permission::JoinVoice => crate::i18n::t("Join voice"),
        Permission::Speak => crate::i18n::t("Speak in voice"),
        Permission::MuteMembers => crate::i18n::t("Mute members"),
        Permission::DeafenMembers => crate::i18n::t("Deafen members"),
        Permission::MoveMembers => crate::i18n::t("Move & disconnect members"),
    }
}
#[component]
fn PermissionChecks(value: RwSignal<Permissions>) -> impl IntoView {
    view! {<div>{Permission::ALL.into_iter().map(move |p|view!{<label class="settings-permission-row"><span>{move ||permission_name(p)}</span><input type="checkbox" prop:checked=move ||value.get().contains(&p) on:change=move |e|value.update(|v|{if event_target_checked(&e){v.insert(p);}else{v.remove(&p);}})/></label>}).collect_view()}</div>}
}

#[component]
pub(super) fn PermissionEditor(ui: Ui) -> impl IntoView {
    let editor = editor(ui);
    if let Some(home) = ui.server.get_untracked().filter(|s| s.can_manage_roles) {
        editor.request(PermissionRequest::Inspect {
            guild_id: home.guild.id,
        });
    }
    view! {<Show when=move ||editor.state.get().is_some() fallback=move ||view!{<section class="settings-card"><p class="settings-hint">{move ||if ui.busy.get(){crate::i18n::t("Loading server settings…")}else{crate::i18n::t("Server settings are unavailable. Your permissions may have changed.")}}</p><button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|if let Some(s)=ui.server.get_untracked(){editor.request(PermissionRequest::Inspect{guild_id:s.guild.id});}>{move || crate::i18n::t("Retry")}</button></section>}>
        <fieldset disabled=move ||ui.busy.get() class="space-y-5 disabled:opacity-60">
            <Show when=move ||matches!(ui.page.get(),"permissions"|"server_overview")><Overview editor=editor/></Show>
            <Show when=move ||ui.page.get()=="server_roles"><Roles editor=editor/></Show>
            <Show when=move ||ui.page.get()=="server_members"><Members editor=editor/></Show>
            <Show when=move ||ui.page.get()=="server_channels"><Channels editor=editor/></Show>
            <Show when=move ||ui.page.get()=="server_danger"><Ownership editor=editor/></Show>
        </fieldset>
    </Show>}
}

#[component]
fn Overview(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let name = RwSignal::new(String::new());
    let saved = Memo::new(move |_| editor.state.get().map(|s| s.guild.name).unwrap_or_default());
    Effect::new(move |_| name.set(saved.get()));
    Effect::new(move |_| ui.settings_dirty.set(name.get() != saved.get()));
    on_cleanup(move || ui.settings_dirty.set(false));
    view! {
        <div class="settings-card"><h3>{move || crate::i18n::t("Your community")}</h3><div class="settings-info-row"><span>{move || crate::i18n::t("Server")}</span><strong>{saved}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Members")}</span><strong>{move ||editor.state.get().map(|s|s.members.len())}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Channels")}</span><strong>{move ||editor.state.get().map(|s|s.channels.len())}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Server ID")}</span><strong class="text-xs">{move ||editor.state.get().map(|s|s.guild.id.to_string())}</strong></div><p class="settings-hint">{move || crate::i18n::t("Share the server ID with people you want to invite. Protected servers also require a password.")}</p></div>
        <Show when=move ||editor.allowed(Permission::ManageGuild)><form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();editor.change(GuildChange::Rename{name:name.get_untracked()});}><h3>{move || crate::i18n::t("Server name")}</h3><Field label="Name" value=name/><div class="settings-actions"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|name.set(saved.get_untracked())>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Save changes")}</button></div></form></Show>
    }
}

#[component]
fn Roles(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let role = editor.selected_role;
    let name = RwSignal::new(String::new());
    let position = RwSignal::new("1".to_owned());
    let grants = RwSignal::new(Permissions::new());
    let baseline = Memo::new(move |_| {
        editor
            .state
            .get()
            .and_then(|s| s.roles.into_iter().find(|r| r.id.to_string() == role.get()))
            .map(|r| (r.name, r.position.to_string(), r.permissions))
            .unwrap_or_else(|| (String::new(), "1".into(), Permissions::new()))
    });
    let reset = move || {
        let (n, p, g) = baseline.get_untracked();
        name.set(n);
        position.set(p);
        grants.set(g);
    };
    Effect::new(move |_| {
        baseline.track();
        reset();
    });
    Effect::new(move |_| {
        ui.settings_dirty
            .set((name.get(), position.get(), grants.get()) != baseline.get())
    });
    on_cleanup(move || ui.settings_dirty.set(false));
    view! {<div class="settings-split"><div class="settings-item-list" aria-label=move || crate::i18n::t("Server roles")><button aria-pressed=move ||role.get().is_empty() disabled=move ||ui.settings_dirty.get() on:click=move |_|role.set(String::new())>{move || crate::i18n::t("＋ New role")}</button>{move ||editor.state.get().map(|s|s.roles.into_iter().map(|r|{let id=r.id.to_string();let selected=id.clone();view!{<button aria-pressed=move ||role.get()==selected disabled=move ||ui.settings_dirty.get() on:click=move |_|role.set(id.clone())>{r.name}</button>}}).collect_view())}</div>
        <form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();let Ok(position)=position.get_untracked().parse()else{ui.status.set("Enter a valid rank".into());return;};let change=if let Ok(role_id)=role.get_untracked().parse(){GuildChange::EditRole{role_id,name:name.get_untracked(),position,permissions:grants.get_untracked()}}else{GuildChange::CreateRole{name:name.get_untracked(),position,permissions:grants.get_untracked()}};editor.change(change);}>
            <h3>{move ||if role.get().is_empty(){crate::i18n::t("Create role")}else{crate::i18n::t("Edit role")}}</h3><Field label="Role name" value=name/><Field label="Hierarchy rank" value=position kind="number"/><p class="settings-hint">{move || crate::i18n::t("Higher ranks can manage lower ranks. Everyone stays at rank 0. You can only grant permissions you already have.")}</p>
            <details><summary>{move || crate::i18n::t("Permissions")}</summary><PermissionChecks value=grants/></details>
            <Show when=move ||ui.settings_dirty.get()><p class="settings-hint">{move || crate::i18n::t("Unsaved changes. Save or reset before selecting another role.")}</p></Show><div class="settings-actions"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|reset()>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Save role")}</button></div>
            <Show when=move ||editor.state.get().is_some_and(|s|s.roles.iter().any(|r|!r.everyone&&r.id.to_string()==role.get()))><super::settings::ConfirmButton ui=ui label="Delete role" title="Delete this role?" detail="Members will lose the permissions this role grants." on_confirm=Callback::new(move |()|if let Ok(role_id)=role.get_untracked().parse(){editor.change(GuildChange::DeleteRole{role_id});role.set(String::new());})/></Show>
        </form>
    </div>}
}

#[component]
fn Members(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let username = RwSignal::new(String::new());
    let selected = RwSignal::new(
        ui.role_target
            .get_untracked()
            .map(|(_, id)| id.to_string())
            .unwrap_or_default(),
    );
    let member = Signal::derive(move || {
        editor.state.get().and_then(|s| {
            s.members
                .into_iter()
                .find(|m| m.account_id.to_string() == selected.get())
        })
    });
    let lower = move || {
        editor
            .state
            .get()
            .zip(ui.account.get())
            .zip(member.get())
            .is_some_and(|((s, a), m)| {
                thiscord_frontend::sidebar::lower_member(&s, a.id, m.account_id)
            })
    };
    view! {
        <Show when=move ||editor.allowed(Permission::ManageInvites)><form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();editor.change(GuildChange::AddMember{username:username.get_untracked()});}><h3>{move || crate::i18n::t("Add a member")}</h3><p class="settings-hint">{move || crate::i18n::t("Enter the username of an existing verified account. Requires permission to add members.")}</p><Field label="Username" value=username/><button class="settings-primary" disabled=move ||username.get().trim().is_empty()>{move || crate::i18n::t("Add member")}</button></form></Show>
        <section class="settings-card"><label class="settings-field"><span>{move || crate::i18n::t("Member")}</span><select prop:value=move ||selected.get() on:change=move |e|selected.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a member")}</option>{move ||editor.state.get().map(|s|s.members.into_iter().map(|m|view!{<option value=m.account_id.to_string()>{format!("{}{}",m.display_name(),if m.account_id==s.guild.owner{crate::i18n::t(" · Owner")}else{""})}</option>}).collect_view())}</select></label>
            <Show when=move ||member.get().is_some()><h3>{move ||member.get().map(|m|m.display_name().to_owned())}</h3><p class="settings-hint">{move || crate::i18n::t("Role changes save immediately. Everyone applies to every member.")}</p>
                {move ||editor.state.get().map(|s|s.roles.into_iter().map(move |r|{let role_id=r.id;view!{<label class="settings-permission-row"><span>{r.name}</span><input type="checkbox" disabled=move ||r.everyone||!lower() prop:checked=move ||r.everyone||member.get().is_some_and(|m|m.roles.contains(&role_id)) on:change=move |e|if let Some(m)=member.get_untracked(){editor.change(GuildChange::AssignRole{account_id:m.account_id,role_id,assigned:event_target_checked(&e)});}/></label>}}).collect_view())}
                <Show when=move ||!lower()><p class="settings-hint">{move || crate::i18n::t("The owner, your own account, and members at or above your highest role are protected.")}</p></Show>
                <Show when=move ||ui.server.get().is_some_and(|s|s.can_moderate)><button class="settings-secondary" on:click=move |_|{ui.moderation_target.set(member.get_untracked().map(|m|m.account_id));super::settings::navigate(ui,"moderation");}>{move || crate::i18n::t("Moderation options")}</button></Show>
            </Show>
        </section>
    }
}

#[component]
fn Channels(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let name = RwSignal::new(String::new());
    let voice = RwSignal::new(false);
    let channel = RwSignal::new(
        ui.permission_channel
            .get_untracked()
            .map(|(_, c)| c.to_string())
            .unwrap_or_default(),
    );
    let preview_member = RwSignal::new(
        ui.account
            .get_untracked()
            .map(|a| a.id.to_string())
            .unwrap_or_default(),
    );
    let target_id = RwSignal::new(String::new());
    let allow = RwSignal::new(Permissions::new());
    let deny = RwSignal::new(Permissions::new());
    let target = move || {
        let value = target_id.get();
        if let Some(id) = value.strip_prefix("member:") {
            id.parse().ok().map(OverrideTarget::Member)
        } else {
            value.parse().ok().map(OverrideTarget::Role)
        }
    };
    let baseline = Memo::new(move |_| {
        editor
            .state
            .get()
            .and_then(|s| {
                s.overrides.into_iter().find(|o| {
                    o.channel_id.to_string() == channel.get() && Some(o.target) == target()
                })
            })
            .map(|o| (o.allow, o.deny))
            .unwrap_or_default()
    });
    let reset = move || {
        let (a, d) = baseline.get_untracked();
        allow.set(a);
        deny.set(d);
    };
    Effect::new(move |_| {
        baseline.track();
        reset();
    });
    Effect::new(move |_| {
        ui.settings_dirty
            .set((allow.get(), deny.get()) != baseline.get() || !name.get().is_empty())
    });
    on_cleanup(move || ui.settings_dirty.set(false));
    view! {
        <Show when=move ||editor.allowed(Permission::ManageChannels)><details class="settings-card"><summary>{move || crate::i18n::t("Create a channel")}</summary><form class="space-y-4" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();editor.change_then(GuildChange::CreateChannel{name:name.get_untracked(),kind:if voice.get_untracked(){ChannelKind::Voice}else{ChannelKind::Text}},Some(Callback::new(move |()|name.set(String::new()))));}><Field label="Channel name" value=name/><label class="settings-field"><span>{move || crate::i18n::t("Type")}</span><select on:change=move |e|voice.set(event_target_value(&e)=="voice")><option value="text">{move || crate::i18n::t("Text")}</option><option value="voice">{move || crate::i18n::t("Voice")}</option></select></label><button class="settings-primary" disabled=move ||name.get().trim().is_empty()>{move || crate::i18n::t("Create channel")}</button></form></details></Show>
        <section class="settings-card"><label class="settings-field"><span>{move || crate::i18n::t("Channel")}</span><select disabled=move ||ui.settings_dirty.get() prop:value=move ||channel.get() on:change=move |e|channel.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a channel")}</option>{move ||editor.state.get().map(|s|s.channels.into_iter().map(|c|view!{<option value=c.id.to_string()>{format!("{} {}",if c.kind==ChannelKind::Voice{"◖"}else{"#"},c.name)}</option>}).collect_view())}</select></label>
            <Show when=move ||!channel.get().is_empty()>
                <h3>{move || crate::i18n::t("Who can access this channel?")}</h3><p class="settings-hint">{move || crate::i18n::t("Inherit uses the member’s server roles. Allow or deny changes a permission in this channel only.")}</p>
                <label class="settings-field"><span>{move || crate::i18n::t("Role or member")}</span><select prop:value=move ||target_id.get() disabled=move ||ui.settings_dirty.get() on:change=move |e|target_id.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a role or member")}</option>{move ||editor.state.get().map(|s|view!{<optgroup label=move || crate::i18n::t("Roles")>{s.roles.into_iter().map(|r|view!{<option value=r.id.to_string()>{r.name}</option>}).collect_view()}</optgroup><optgroup label=move || crate::i18n::t("Members")>{s.members.into_iter().map(|m|view!{<option value=format!("member:{}",m.account_id)>{m.display_name().to_owned()}</option>}).collect_view()}</optgroup>})}</select></label>
                <Show when=move ||target().is_some()><form on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();if let (Ok(channel_id),Some(target))=(channel.get_untracked().parse(),target()){editor.change(GuildChange::SetOverride{channel_id,target,allow:allow.get_untracked(),deny:deny.get_untracked()});}}>
                    {Permission::ALL.into_iter().filter(|p|*p!=Permission::Administrator).map(move |p|view!{<label class="settings-permission-row"><span>{move ||permission_name(p)}</span><select aria-label=move ||permission_name(p) prop:value=move ||if allow.get().contains(&p){"allow"}else if deny.get().contains(&p){"deny"}else{"inherit"} on:change=move |e|{let v=event_target_value(&e);allow.update(|a|{a.remove(&p);if v=="allow"{a.insert(p);}});deny.update(|d|{d.remove(&p);if v=="deny"{d.insert(p);}});} ><option value="inherit">{move || crate::i18n::t("Inherit")}</option><option value="allow">{move || crate::i18n::t("Allow")}</option><option value="deny">{move || crate::i18n::t("Deny")}</option></select></label>}).collect_view()}
                    <div class="settings-actions mt-5"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|reset()>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()>{move || crate::i18n::t("Save permissions")}</button></div>
                    <button type="button" class="settings-link mt-4" on:click=move |_|if let (Ok(channel_id),Some(target))=(channel.get_untracked().parse(),target()){editor.change(GuildChange::DeleteOverride{channel_id,target});}>{move || crate::i18n::t("Remove this override")}</button>
                </form></Show>
                <details><summary>{move || crate::i18n::t("Check a member’s access")}</summary><label class="settings-field"><span>{move || crate::i18n::t("Member")}</span><select prop:value=move ||preview_member.get() on:change=move |e|preview_member.set(event_target_value(&e))>{move ||editor.state.get().map(|s|s.members.into_iter().map(|m|view!{<option value=m.account_id.to_string()>{m.display_name().to_owned()}</option>}).collect_view())}</select></label><button class="settings-secondary mt-3" on:click=move |_|if let (Some(s),Ok(account_id),Ok(channel_id))=(editor.state.get_untracked(),preview_member.get_untracked().parse::<AccountId>(),channel.get_untracked().parse()){editor.request(PermissionRequest::Preview{guild_id:s.guild.id,account_id,channel_id:Some(channel_id)});}>{move || crate::i18n::t("Check access")}</button><p class="settings-hint mt-3" role="status">{move ||editor.preview.get()}</p></details>
                <Show when=move ||editor.allowed(Permission::ManageChannels)><super::settings::ConfirmButton ui=ui label="Delete channel" title="Delete this channel?" detail="The channel and its messages will be permanently deleted." on_confirm=Callback::new(move |()|if let Ok(channel_id)=channel.get_untracked().parse(){editor.change(GuildChange::DeleteChannel{channel_id});channel.set(String::new());})/></Show>
            </Show>
        </section>
    }
}

#[component]
fn Ownership(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let member = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let confirmed = move || {
        editor
            .state
            .get()
            .is_some_and(|s| confirmation.get() == s.guild.name)
    };
    view! {<Show when=move ||editor.state.get().is_some_and(|s|ui.account.get().is_some_and(|a|a.id==s.guild.owner))>
        <div class="settings-card"><h3>{move || crate::i18n::t("Before you continue")}</h3><p class="settings-hint">{move || crate::i18n::t("Ownership changes and deletion require a recent identity confirmation.")}</p><button class="settings-secondary" on:click=move |_|super::settings::navigate(ui,"reauthenticate")>{move || crate::i18n::t("Confirm identity")}</button></div>
        <section class="settings-card settings-danger-zone"><h3>{move || crate::i18n::t("Transfer ownership")}</h3><p class="settings-hint">{move || crate::i18n::t("The new owner gets full control. You remain a member and lose owner privileges.")}</p><label class="settings-field"><span>{move || crate::i18n::t("New owner")}</span><select prop:value=move ||member.get() on:change=move |e|member.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a member")}</option>{move ||editor.state.get().map(|s|s.members.into_iter().filter(|m|m.account_id!=s.guild.owner).map(|m|view!{<option value=m.account_id.to_string()>{m.display_name().to_owned()}</option>}).collect_view())}</select></label><super::settings::ConfirmButton ui=ui label="Transfer ownership" title="Transfer this server’s ownership?" detail="You will lose owner privileges. This requires recent authentication." disabled=Signal::derive(move ||member.get().is_empty()) on_confirm=Callback::new(move |()|if let Ok(account_id)=member.get_untracked().parse(){editor.change(GuildChange::TransferOwner{account_id});})/></section>
        <section class="settings-card settings-danger-zone"><h3>{move || crate::i18n::t("Delete server")}</h3><p class="settings-hint">{move || crate::i18n::t("Permanently deletes this server, its channels and all messages. This cannot be undone.")}</p><p class="settings-hint">{move ||editor.state.get().map(|s|thiscord_frontend::localized_format!(crate::i18n::language(), "Type {} to confirm.",s.guild.name))}</p><Field label="Server name" value=confirmation/><super::settings::ConfirmButton ui=ui label="Delete server" title="Permanently delete this server?" detail="All channels, messages and memberships will be removed. This cannot be undone." disabled=Signal::derive(move ||!confirmed()) on_confirm=Callback::new(move |()|editor.change(GuildChange::Delete{}))/></section>
    </Show>}
}

#[component]
pub(super) fn InstanceSettings(ui: Ui) -> impl IntoView {
    let editor = editor(ui);
    let target = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    editor.request(PermissionRequest::Instance {});
    view! {<section class="settings-card"><h3>{move || crate::i18n::t("Instance access")}</h3><p class="settings-hint">{move || crate::i18n::t("Instance administrators can create servers. Their role does not grant access to other people’s servers.")}</p><p class="settings-hint">{move ||editor.access.get().map(|a|thiscord_frontend::localized_format!(crate::i18n::language(), "Your instance role: {}",match a.role{InstanceRole::Owner=>crate::i18n::t("Owner"),InstanceRole::Admin=>crate::i18n::t("Administrator"),InstanceRole::User=>crate::i18n::t("Member")}))}</p>
        <Show when=move ||editor.access.get().is_some_and(|a|a.role==InstanceRole::Owner)><Field label="Verified account ID" value=target/><div class="settings-actions"><button class="settings-secondary" disabled=move ||{ui.busy.get()||target.get().parse::<AccountId>().is_err()} on:click=move |_|if let Ok(account_id)=target.get_untracked().parse(){editor.request(PermissionRequest::SetInstanceAdmin{account_id,admin:true});}>{move || crate::i18n::t("Make administrator")}</button><button class="settings-secondary" disabled=move ||{ui.busy.get()||target.get().parse::<AccountId>().is_err()} on:click=move |_|if let Ok(account_id)=target.get_untracked().parse(){editor.request(PermissionRequest::SetInstanceAdmin{account_id,admin:false});}>{move || crate::i18n::t("Remove administrator")}</button></div>
        <details class="settings-danger-zone"><summary>{move || crate::i18n::t("Transfer instance ownership")}</summary><p class="settings-hint">{move || crate::i18n::t("Requires recent authentication. You will become a regular instance member. Type TRANSFER INSTANCE to continue.")}</p><Field label="Confirmation" value=confirmation/><super::settings::ConfirmButton ui=ui label="Transfer instance" title="Transfer instance ownership?" detail="The selected account becomes the instance owner. You lose owner privileges." disabled=Signal::derive(move ||confirmation.get()!="TRANSFER INSTANCE"||target.get().parse::<AccountId>().is_err()) on_confirm=Callback::new(move |()|if let Ok(account_id)=target.get_untracked().parse(){editor.request(PermissionRequest::TransferInstance{account_id});})/></details></Show>
    </section>}
}
