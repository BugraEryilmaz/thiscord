#[path = "organization.rs"]
mod organization;
use super::{
    Field, Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
pub(super) use organization::{InvitesPanel, OrganizationPanel};
use thiscord_shared::{
    AccountId, InstanceRoleId,
    pagination::{Page, PageRequest},
    permissions::*,
};

#[derive(Clone)]
struct InstanceMembersPage {
    role_id: InstanceRoleId,
    request: PageRequest,
    page: Page<InstanceRoleMember>,
}

#[derive(Clone, Copy)]
struct Editor {
    ui: Ui,
    guilds: RwSignal<Vec<Guild>>,
    state: RwSignal<Option<GuildState>>,
    access: RwSignal<Option<InstanceAccess>>,
    preview: RwSignal<String>,
    selected_role: RwSignal<String>,
    selected_instance_role: RwSignal<String>,
    instance_members: RwSignal<Option<InstanceMembersPage>>,
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
        if let Some(outcome) = self.submit(command, on_success) {
            leptos::task::spawn_local(async move {
                let _ = outcome.await;
            });
        }
    }
    /// Starts a command unless another one is running. Resolves with the error
    /// that was also reported in the status bar.
    fn submit(
        self,
        command: PermissionRequest,
        on_success: Option<Callback<()>>,
    ) -> Option<impl std::future::Future<Output = Result<(), String>> + 'static> {
        if self.ui.busy.get_untracked() {
            return None;
        }
        self.ui.busy.set(true);
        let instance_mutation = matches!(
            &command,
            PermissionRequest::SetInstanceAdmin { .. }
                | PermissionRequest::TransferInstance { .. }
                | PermissionRequest::SetInstanceRoleLimits { .. }
                | PermissionRequest::CreateInstanceRole { .. }
                | PermissionRequest::DeleteInstanceRole { .. }
                | PermissionRequest::SetInstanceRole { .. }
        );
        let mutation = instance_mutation
            || matches!(
                &command,
                PermissionRequest::Change { .. }
                    | PermissionRequest::SetInstanceAdmin { .. }
                    | PermissionRequest::TransferInstance { .. }
                    | PermissionRequest::SetInstanceRoleLimits { .. }
            );
        let creating_role = matches!(
            &command,
            PermissionRequest::Change {
                change: GuildChange::CreateRole { .. },
                ..
            }
        );
        Some(async move {
            let mut outcome = Ok(());
            let selection = self.ui.navigation.get_untracked().current();
            let token = self.ui.token.get_untracked();
            let result = crate::account_client::api_request::<_, PermissionResponse>(
                &permissions_path(),
                &command,
                token.as_deref(),
            )
            .await;
            if self.ui.token.get_untracked() != token
                || self.state.is_disposed()
                || !super::servers::current(self.ui, selection)
            {
                self.ui.busy.set(false);
                return outcome;
            }
            match result {
                Ok(response) => {
                    self.ui.status.set(String::new());
                    match response {
                        PermissionResponse::Moderation { .. }
                        | PermissionResponse::Invites { .. }
                        | PermissionResponse::InviteCreated { .. }
                        | PermissionResponse::Organization { .. }
                        | PermissionResponse::AuditLog { .. } => {}
                        PermissionResponse::Joined { .. } | PermissionResponse::Home { .. } => {}
                        PermissionResponse::Instance { access } => {
                            self.instance_members.set(None);
                            self.ui.instance_access.set(Some(access.clone()));
                            self.access.set(Some(access));
                        }
                        PermissionResponse::InstanceRoleMembers { role_id, page } => {
                            if let PermissionRequest::ListInstanceRoleMembers {
                                role_id: requested,
                                page: request,
                            } = &command
                                && *requested == role_id
                                && self.selected_instance_role.get_untracked()
                                    == role_id.to_string()
                                && self
                                    .access
                                    .get_untracked()
                                    .is_some_and(|a| a.role == InstanceRole::Owner)
                            {
                                self.instance_members.set(Some(InstanceMembersPage {
                                    role_id,
                                    request: request.clone(),
                                    page,
                                }));
                            }
                        }
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
                        if instance_mutation {
                            self.instance_members.set(None);
                        }
                        super::servers::refresh(self.ui).await;
                    }
                    if self.ui.token.get_untracked() != token
                        || self.state.is_disposed()
                        || !super::servers::current(self.ui, selection)
                    {
                        self.ui.busy.set(false);
                        return outcome;
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
                Err(error) => {
                    outcome = Err(error.clone());
                    // Reload the authoritative policies after rejected writes, including
                    // stale revisions. Never retry a mutation with a new revision.
                    if instance_mutation {
                        let refreshed =
                            crate::account_client::api_request::<_, PermissionResponse>(
                                &permissions_path(),
                                &PermissionRequest::Instance {},
                                token.as_deref(),
                            )
                            .await;
                        if self.ui.token.get_untracked() != token
                            || self.access.is_disposed()
                            || !super::servers::current(self.ui, selection)
                        {
                            self.ui.busy.set(false);
                            return outcome;
                        }
                        if let Ok(PermissionResponse::Instance { access }) = refreshed {
                            self.instance_members.set(None);
                            self.ui.instance_access.set(Some(access.clone()));
                            self.access.set(Some(access));
                            self.ui.status.set(thiscord_frontend::localized_format!(
                                crate::i18n::language(),
                                "{error}. Instance roles have been refreshed and edits reset. Review before trying again.",
                                error = error
                            ));
                        } else {
                            self.ui.status.set(thiscord_frontend::localized_format!(
                                crate::i18n::language(),
                                "{error}. Could not refresh instance roles. Reset your edits and refresh before trying again.",
                                error = error
                            ));
                        }
                    } else {
                        self.ui.status.set(error);
                    }
                }
            }
            self.ui.busy.set(false);
            outcome
        })
    }
    fn change(self, change: GuildChange) {
        self.change_then(change, None);
    }
    /// Guild change for a confirmation dialog, using the revision current at confirmation.
    fn confirmed(
        self,
        change: impl Fn() -> Option<GuildChange> + 'static,
    ) -> super::dialog::Action {
        self.confirmed_inner(change, None)
    }
    /// Non-guild command for a confirmation dialog.
    fn confirmed_request(
        self,
        command: impl Fn() -> Option<PermissionRequest> + 'static,
    ) -> super::dialog::Action {
        super::dialog::Action::new(move |_| {
            let outcome = command().and_then(|command| self.submit(command, None));
            async move {
                match outcome {
                    Some(outcome) => outcome.await,
                    None => Err(super::dialog::busy_error()),
                }
            }
        })
    }
    fn confirmed_then(
        self,
        change: impl Fn() -> Option<GuildChange> + 'static,
        on_success: Callback<()>,
    ) -> super::dialog::Action {
        self.confirmed_inner(change, Some(on_success))
    }
    fn confirmed_inner(
        self,
        change: impl Fn() -> Option<GuildChange> + 'static,
        on_success: Option<Callback<()>>,
    ) -> super::dialog::Action {
        super::dialog::Action::new(move |_| {
            let outcome = change().and_then(|change| {
                let state = self.state.get_untracked()?;
                self.submit(
                    PermissionRequest::Change {
                        guild_id: state.guild.id,
                        revision: state.guild.revision,
                        change,
                    },
                    on_success,
                )
            });
            async move {
                match outcome {
                    Some(outcome) => outcome.await,
                    None => Err(super::dialog::busy_error()),
                }
            }
        })
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
        selected_instance_role: RwSignal::new(String::new()),
        instance_members: RwSignal::new(None),
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
        Permission::UseSoundboard => crate::i18n::t("Use soundboard"),
        Permission::ChangeNickname => crate::i18n::t("Change own nickname"),
        Permission::ManageNicknames => crate::i18n::t("Manage nicknames"),
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
        <CapacitySummary editor=editor/>
        <div class="settings-card"><h3>{move || crate::i18n::t("Your community")}</h3><div class="settings-info-row"><span>{move || crate::i18n::t("Server")}</span><strong>{saved}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Members")}</span><strong>{move ||editor.state.get().map(|s|s.members.len())}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Channels")}</span><strong>{move ||editor.state.get().map(|s|s.channels.len())}</strong></div><div class="settings-info-row"><span>{move || crate::i18n::t("Server ID")}</span><strong class="text-xs">{move ||editor.state.get().map(|s|s.guild.id.to_string())}</strong></div><p class="settings-hint">{move || crate::i18n::t("Use Copy invite link beside the server name to invite people. Manage and revoke links in Invite links.")}</p></div>
        <Show when=move ||editor.allowed(Permission::ManageGuild)><ServerIcon editor=editor/></Show>
        <Show when=move ||editor.allowed(Permission::ManageGuild)><form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();editor.change(GuildChange::Rename{name:name.get_untracked()});}><h3>{move || crate::i18n::t("Server name")}</h3><Field label="Name" value=name/><div class="settings-actions"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|name.set(saved.get_untracked())>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Save changes")}</button></div></form></Show>
        <Show when=move ||editor.allowed(Permission::ManageGuild)><JoinPassword editor=editor/></Show>
    }
}

/// The password people need to join by server ID. Invite links skip it.
#[component]
fn JoinPassword(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let password = RwSignal::new(String::new());
    let visible = RwSignal::new(false);
    let protected = Memo::new(move |_| editor.state.get().is_some_and(|s| s.password_protected));
    let too_long = move || password.with(|p| p.len() > MAX_GUILD_PASSWORD_BYTES);
    let cleared = Callback::new(move |()| {
        password.set(String::new());
        visible.set(false);
    });
    let save = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let value = password.get_untracked();
        if value.is_empty() || too_long() {
            return;
        }
        editor.change_then(
            GuildChange::SetPassword {
                password: Some(GuildPassword(value)),
            },
            Some(cleared),
        );
    };
    view! {
        <form class="settings-card" on:submit=save aria-labelledby="join-password-title">
            <div class="join-password-head">
                <span class="join-password-icon" class:protected=move || protected.get() aria-hidden="true"><Icon kind=Glyph::Lock/></span>
                <div class="min-w-0 flex-1">
                    <h3 id="join-password-title">{move || crate::i18n::t("Join password")}</h3>
                    <p class="settings-hint">{move || if protected.get() {
                        crate::i18n::t("On. People joining with the server ID must enter this password.")
                    } else {
                        crate::i18n::t("Off. Anyone with the server ID can join.")
                    }}</p>
                </div>
            </div>
            <label class="field">
                <span class="field-label">{move || if protected.get() { crate::i18n::t("New password") } else { crate::i18n::t("Password") }}</span>
                <div class="join-password-input">
                    <input class="field-input" type=move || if visible.get() { "text" } else { "password" } autocomplete="new-password"
                        spellcheck="false" autocapitalize="off" placeholder=move || if protected.get() { crate::i18n::t("Enter a new password") } else { crate::i18n::t("Choose a password") }
                        prop:value=move || password.get() on:input=move |e| password.set(event_target_value(&e))/>
                    <button type="button" class="join-password-reveal" aria-pressed=move || visible.get().to_string()
                        aria-label=move || if visible.get() { crate::i18n::t("Hide password") } else { crate::i18n::t("Show password") }
                        on:click=move |_| visible.update(|v| *v = !*v)>
                        {move || if visible.get() { view! {<Icon kind=Glyph::EyeOff/>} } else { view! {<Icon kind=Glyph::Eye/>} }}
                    </button>
                </div>
            </label>
            <Show when=too_long><p class="identity-error" role="alert">{move || crate::i18n::t("Server password must be at most 1024 bytes")}</p></Show>
            <p class="settings-hint">{move || crate::i18n::t("Share it only with people you want here. Invite links still work without it, and current members are never removed.")}</p>
            <div class="settings-actions">
                <Show when=move || protected.get()>
                    <super::settings::ConfirmButton ui=ui label="Remove password" title="Remove the join password?" detail="Anyone with the server ID will be able to join. Invite links and current members are not affected."
                        target=Signal::derive(move || editor.state.get().map(|s| s.guild.name).unwrap_or_default())
                        action=editor.confirmed_then(|| Some(GuildChange::SetPassword { password: None }), cleared)/>
                </Show>
                <button type="submit" class="settings-primary" disabled=move || password.get().is_empty() || too_long()>
                    {move || if protected.get() { crate::i18n::t("Change password") } else { crate::i18n::t("Turn on password") }}
                </button>
            </div>
        </form>
    }
}

/// Uploads (Some) or removes (None) the guild icon at the editor's current revision.
async fn set_icon(editor: Editor, image_base64: Option<String>) -> Result<(), String> {
    let ui = editor.ui;
    let Some(state) = editor.state.get_untracked() else {
        return Err("Server settings are unavailable. Your permissions may have changed.".into());
    };
    if ui.busy.get_untracked() {
        return Err(super::dialog::busy_error());
    }
    ui.busy.set(true);
    let removing = image_base64.is_none();
    let selection = ui.navigation.get_untracked().current();
    let token = ui.token.get_untracked();
    ui.status.set(
        if removing {
            "Removing server icon…"
        } else {
            "Uploading server icon…"
        }
        .into(),
    );
    let result = crate::account_client::api_request::<_, Guild>(
        GUILD_ICON_PATH,
        &SetGuildIcon {
            guild_id: state.guild.id,
            revision: state.guild.revision,
            image_base64,
        },
        token.as_deref(),
    )
    .await;
    if ui.token.get_untracked() != token
        || editor.state.is_disposed()
        || !super::servers::current(ui, selection)
    {
        ui.busy.set(false);
        return result.map(|_| ());
    }
    let outcome = match result {
        Ok(guild) => {
            editor.state.update(|current| {
                if let Some(current) = current
                    && current.guild.id == guild.id
                {
                    current.guild = guild;
                }
            });
            ui.status.set(
                if removing {
                    "Server icon removed"
                } else {
                    "Server icon saved"
                }
                .into(),
            );
            Ok(())
        }
        Err(error) => {
            ui.status.set(error.clone());
            Err(error)
        }
    };
    super::servers::refresh(ui).await;
    ui.busy.set(false);
    // A stale revision means someone else changed the server: reload it.
    if outcome.is_err() && !editor.state.is_disposed() {
        editor.request(PermissionRequest::Inspect {
            guild_id: state.guild.id,
        });
    }
    outcome
}

#[component]
fn ServerIcon(editor: Editor) -> impl IntoView {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let ui = editor.ui;
    let name = Signal::derive(move || editor.state.get().map(|s| s.guild.name).unwrap_or_default());
    let icon = Signal::derive(move || editor.state.get().and_then(|s| s.guild.icon_id));
    let choose = move |event| {
        let input = event_target::<web_sys::HtmlInputElement>(&event);
        let file = input.files().and_then(|files| files.get(0));
        input.set_value(""); // Selecting the same file again should retry a failed upload.
        let Some(file) = file else {
            return;
        };
        if file.size() == 0.0 || file.size() > MAX_GUILD_ICON_BYTES as f64 {
            ui.status.set("Choose a picture up to 2 MiB".into());
            return;
        }
        leptos::task::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let encoded = STANDARD.encode(js_sys::Uint8Array::new(&buffer).to_vec());
                    let _ = set_icon(editor, Some(encoded)).await;
                }
                Err(_) => ui
                    .status
                    .set("Cannot read this picture. Please choose it again".into()),
            }
        });
    };
    let remove = move |_| {
        super::dialog::confirm(
            ui,
            super::dialog::Confirmation::new(
                crate::i18n::t("Remove the server icon?"),
                crate::i18n::t("Members will see the server’s initials instead. You can upload a new icon at any time."),
                crate::i18n::t("Remove icon"),
                super::dialog::Action::new(move |_| set_icon(editor, None)),
            )
            .target(name.get_untracked()),
        );
    };
    view! {
        <section class="settings-card" aria-label=move || crate::i18n::t("Server icon")>
            <h3>{move || crate::i18n::t("Server icon")}</h3>
            <div class="flex flex-wrap items-center gap-4">
                <span class="relative grid size-20 shrink-0 place-items-center overflow-hidden rounded-2xl bg-white/10 text-2xl font-semibold">
                    <crate::avatar::GuildIcon name=name icon_id=icon/>
                </span>
                <div class="space-y-2">
                    <label class="block space-y-2 text-sm">
                        <span class="settings-secondary">{move || if icon.get().is_some() { crate::i18n::t("Change icon") } else { crate::i18n::t("Upload icon") }}</span>
                        <input class="sr-only" type="file" accept="image/png,image/jpeg,image/webp" disabled=move || ui.busy.get() on:change=choose/>
                    </label>
                    <button type="button" class="settings-link disabled:opacity-50" disabled=move || ui.busy.get() || icon.get().is_none() on:click=remove>{move || crate::i18n::t("Remove icon")}</button>
                </div>
            </div>
            <p class="settings-hint">{move || crate::i18n::t("PNG, JPEG or WebP, up to 2 MiB. Icons are cropped to a square, save immediately and are visible to server members.")}</p>
        </section>
    }
}

#[component]
fn Roles(editor: Editor) -> impl IntoView {
    let ui = editor.ui;
    let role = editor.selected_role;
    let name = RwSignal::new(String::new());
    let position = RwSignal::new("1".to_owned());
    let grants = RwSignal::new(Permissions::new());
    let appearance = RwSignal::new(RoleAppearance::default());
    let baseline = Memo::new(move |_| {
        editor
            .state
            .get()
            .and_then(|s| s.roles.into_iter().find(|r| r.id.to_string() == role.get()))
            .map(|r| {
                (
                    r.name.clone(),
                    r.position.to_string(),
                    r.permissions.clone(),
                    r.appearance(),
                )
            })
            .unwrap_or_else(|| {
                (
                    String::new(),
                    "1".into(),
                    Permissions::new(),
                    RoleAppearance::default(),
                )
            })
    });
    let everyone = Memo::new(move |_| {
        editor.state.with(|s| {
            s.as_ref().is_some_and(|s| {
                s.roles
                    .iter()
                    .any(|r| r.everyone && r.id.to_string() == role.get())
            })
        })
    });
    let reset = move || {
        let (n, p, g, a) = baseline.get_untracked();
        name.set(n);
        position.set(p);
        grants.set(g);
        appearance.set(a);
    };
    Effect::new(move |_| {
        baseline.track();
        reset();
    });
    Effect::new(move |_| {
        ui.settings_dirty
            .set((name.get(), position.get(), grants.get(), appearance.get()) != baseline.get())
    });
    on_cleanup(move || ui.settings_dirty.set(false));
    let submit = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let Ok(position) = position.get_untracked().parse() else {
            ui.status.set("Enter a valid rank".into());
            return;
        };
        let appearance = Some(appearance.get_untracked());
        let change = if let Ok(role_id) = role.get_untracked().parse() {
            GuildChange::EditRole {
                role_id,
                name: name.get_untracked(),
                position,
                permissions: grants.get_untracked(),
                appearance,
            }
        } else {
            GuildChange::CreateRole {
                name: name.get_untracked(),
                position,
                permissions: grants.get_untracked(),
                appearance,
            }
        };
        editor.change(change);
    };
    view! {<CapacitySummary editor=editor/><div class="settings-split"><div class="settings-item-list" aria-label=move || crate::i18n::t("Server roles")><button aria-pressed=move ||role.get().is_empty() disabled=move ||ui.settings_dirty.get() on:click=move |_|role.set(String::new())>{move || crate::i18n::t("＋ New role")}</button>{move ||editor.state.get().map(|s|s.roles.into_iter().map(|r|{let id=r.id.to_string();let selected=id.clone();view!{<button aria-pressed=move ||role.get()==selected disabled=move ||ui.settings_dirty.get() on:click=move |_|role.set(id.clone())>
            <span class="role-list-dot" class:role-list-dot-none=r.color.is_none() style:background-color=r.color.map(|c|c.hex()) aria-hidden="true"></span><span class="min-w-0 truncate">{r.name}</span>
        </button>}}).collect_view())}</div>
        <form class="settings-card" on:submit=submit>
            <h3>{move ||if role.get().is_empty(){crate::i18n::t("Create role")}else{crate::i18n::t("Edit role")}}</h3><Field label="Role name" value=name/><Field label="Hierarchy rank" value=position kind="number"/><p class="settings-hint">{move || crate::i18n::t("Higher ranks can manage lower ranks. Everyone stays at rank 0. You can only grant permissions you already have.")}</p>
            <Show when=move || !everyone.get() fallback=move || view! {<p class="settings-hint">{move || crate::i18n::t("Everyone has no color and is never shown separately: it applies to every member.")}</p>}>
                <RoleDisplay name appearance/>
            </Show>
            <details><summary>{move || crate::i18n::t("Permissions")}</summary><PermissionChecks value=grants/></details>
            <Show when=move ||ui.settings_dirty.get()><p class="settings-hint">{move || crate::i18n::t("Unsaved changes. Save or reset before selecting another role.")}</p></Show><div class="settings-actions"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|reset()>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()||name.get().trim().is_empty()>{move || crate::i18n::t("Save role")}</button></div>
            <Show when=move ||editor.state.get().is_some_and(|s|s.roles.iter().any(|r|!r.everyone&&r.id.to_string()==role.get()))><super::settings::ConfirmButton ui=ui label="Delete role" title="Delete this role?" detail="Members lose the permissions it grants, and its channel permission overrides are removed. This cannot be undone." target=Signal::derive(move ||editor.state.get().and_then(|s|s.roles.into_iter().find(|r|r.id.to_string()==role.get()).map(|r|r.name)).unwrap_or_default()) action=editor.confirmed_then(move ||role.get_untracked().parse().ok().map(|role_id|GuildChange::DeleteRole{role_id}),Callback::new(move |()|role.set(String::new())))/></Show>
        </form>
    </div>}
}

/// Colour and "show separately" controls with a live preview, part of the
/// role form; saved with the rest of the role.
#[component]
fn RoleDisplay(name: RwSignal<String>, appearance: RwSignal<RoleAppearance>) -> impl IntoView {
    use thiscord_frontend::appearance::{ROLE_COLORS, Rgb};
    let to_role = |c: Rgb| RoleColor::new(u32::from_be_bytes([0, c.0, c.1, c.2]));
    let color = move || appearance.with(|a| a.color);
    let set_color = move |value: Option<RoleColor>| appearance.update(|a| a.color = value);
    let custom =
        move || color().is_some_and(|c| !ROLE_COLORS.iter().any(|(_, p)| to_role(*p) == Some(c)));
    // The roving tab stop: the selected swatch, or "No color" for custom colours.
    let stop = move |swatch: Option<RoleColor>| {
        if color() == swatch || (custom() && swatch.is_none()) {
            "0"
        } else {
            "-1"
        }
    };
    let picked = move |e: &leptos::ev::Event| Rgb::parse(&event_target_value(e)).and_then(to_role);
    let preview_name = move || {
        let n = name.get();
        if n.trim().is_empty() {
            crate::i18n::t("Role name").to_owned()
        } else {
            n
        }
    };
    view! {
        <fieldset class="role-display">
            <legend>{move || crate::i18n::t("Display")}</legend>
            <div class="role-colors" role="radiogroup" aria-label=move || crate::i18n::t("Role color") on:keydown=move |e| super::keyboard::rove_radio(&e)>
                <button type="button" role="radio" class="role-color role-color-none" tabindex=move || stop(None)
                    aria-checked=move || color().is_none().to_string() aria-label=move || crate::i18n::t("No color") data-tip=move || crate::i18n::t("No color")
                    on:click=move |_| set_color(None)></button>
                {ROLE_COLORS.map(|(label, rgb)| {
                    let value = to_role(rgb);
                    view! {
                        <button type="button" role="radio" class="role-color" tabindex=move || stop(value)
                            style:background-color=rgb.hex() aria-checked=move || (color() == value).to_string()
                            aria-label=move || crate::i18n::t(label) data-tip=move || crate::i18n::t(label)
                            on:click=move |_| set_color(value)></button>
                    }
                })}
                <label class="appearance-custom" class:selected=custom data-tip=move || crate::i18n::t("Custom color")>
                    <input type="color" aria-label=move || crate::i18n::t("Custom color")
                        prop:value=move || color().map_or_else(|| "#7c6cf2".to_owned(), |c| c.hex())
                        on:input=move |e| if let Some(value) = picked(&e) { set_color(Some(value)); }/>
                    <span>{move || crate::i18n::t("Custom")}</span>
                </label>
            </div>
            <div class="role-preview" aria-hidden="true">
                <span class="role-preview-label">{move || crate::i18n::t("Preview")}</span>
                <span class="role-preview-name" style:color=move || super::members::name_color(color())>{preview_name}</span>
                <span class="member-badge member-role-badge">
                    {move || color().map(|c| view! {<span class="role-dot" style:background-color=c.hex()></span>})}
                    <span class="truncate">{preview_name}</span>
                </span>
            </div>
            <p class="settings-hint">{move || crate::i18n::t("Members show the color of their highest colored role. Names are adjusted when needed so they stay readable on every theme.")}</p>
            <label class="settings-permission-row role-hoist">
                <span>
                    <strong>{move || crate::i18n::t("Show members separately")}</strong>
                    <small>{move || crate::i18n::t("Online members get a section for their highest role with this on; everyone else is listed under Online.")}</small>
                </span>
                <input type="checkbox" prop:checked=move || appearance.with(|a| a.hoist)
                    on:change=move |e| { let on = event_target_checked(&e); appearance.update(|a| a.hoist = on); }/>
            </label>
        </fieldset>
    }
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
        <CapacitySummary editor=editor/>
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
        <CapacitySummary editor=editor/>
        <Show when=move ||editor.allowed(Permission::ManageChannels)><details class="settings-card"><summary>{move || crate::i18n::t("Create a channel")}</summary><form class="space-y-4" on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();editor.change_then(GuildChange::CreateChannel{name:name.get_untracked(),kind:if voice.get_untracked(){ChannelKind::Voice}else{ChannelKind::Text}},Some(Callback::new(move |()|name.set(String::new()))));}><Field label="Channel name" value=name/><label class="settings-field"><span>{move || crate::i18n::t("Type")}</span><select on:change=move |e|voice.set(event_target_value(&e)=="voice")><option value="text">{move || crate::i18n::t("Text")}</option><option value="voice">{move || crate::i18n::t("Voice")}</option></select></label><button class="settings-primary" disabled=move ||name.get().trim().is_empty()>{move || crate::i18n::t("Create channel")}</button></form></details></Show>
        <section class="settings-card"><label class="settings-field"><span>{move || crate::i18n::t("Channel")}</span><select disabled=move ||ui.settings_dirty.get() prop:value=move ||channel.get() on:change=move |e|channel.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a channel")}</option>{move ||editor.state.get().map(|s|s.channels.into_iter().map(|c|view!{<option value=c.id.to_string()>{format!("{} {}",if c.kind==ChannelKind::Voice{"◖"}else{"#"},c.name)}</option>}).collect_view())}</select></label>
            <Show when=move ||!channel.get().is_empty()>
                <h3>{move || crate::i18n::t("Who can access this channel?")}</h3><p class="settings-hint">{move || crate::i18n::t("Inherit uses the member’s server roles. Allow or deny changes a permission in this channel only.")}</p>
                <label class="settings-field"><span>{move || crate::i18n::t("Role or member")}</span><select prop:value=move ||target_id.get() disabled=move ||ui.settings_dirty.get() on:change=move |e|target_id.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a role or member")}</option>{move ||editor.state.get().map(|s|view!{<optgroup label=move || crate::i18n::t("Roles")>{s.roles.into_iter().map(|r|view!{<option value=r.id.to_string()>{r.name}</option>}).collect_view()}</optgroup><optgroup label=move || crate::i18n::t("Members")>{s.members.into_iter().map(|m|view!{<option value=format!("member:{}",m.account_id)>{m.display_name().to_owned()}</option>}).collect_view()}</optgroup>})}</select></label>
                <Show when=move ||target().is_some()><form on:submit=move |e:leptos::ev::SubmitEvent|{e.prevent_default();if let (Ok(channel_id),Some(target))=(channel.get_untracked().parse(),target()){editor.change(GuildChange::SetOverride{channel_id,target,allow:allow.get_untracked(),deny:deny.get_untracked()});}}>
                    {Permission::ALL.into_iter().filter(|p|*p!=Permission::Administrator).map(move |p|view!{<label class="settings-permission-row"><span>{move ||permission_name(p)}</span><select aria-label=move ||permission_name(p) prop:value=move ||if allow.get().contains(&p){"allow"}else if deny.get().contains(&p){"deny"}else{"inherit"} on:change=move |e|{let v=event_target_value(&e);allow.update(|a|{a.remove(&p);if v=="allow"{a.insert(p);}});deny.update(|d|{d.remove(&p);if v=="deny"{d.insert(p);}});} ><option value="inherit">{move || crate::i18n::t("Inherit")}</option><option value="allow">{move || crate::i18n::t("Allow")}</option><option value="deny">{move || crate::i18n::t("Deny")}</option></select></label>}).collect_view()}
                    <div class="settings-actions mt-5"><button type="button" class="settings-secondary" disabled=move ||!ui.settings_dirty.get() on:click=move |_|reset()>{move || crate::i18n::t("Reset")}</button><button class="settings-primary" disabled=move ||!ui.settings_dirty.get()>{move || crate::i18n::t("Save permissions")}</button></div>
                    <button type="button" class="settings-link mt-4" on:click=move |_|super::dialog::confirm(ui,super::dialog::Confirmation::new(crate::i18n::t("Remove this override?"),crate::i18n::t("This role or member goes back to the server-wide permissions in this channel."),crate::i18n::t("Remove override"),editor.confirmed(move ||channel.get_untracked().parse().ok().zip(target()).map(|(channel_id,target)|GuildChange::DeleteOverride{channel_id,target})))) >{move || crate::i18n::t("Remove this override")}</button>
                </form></Show>
                <details><summary>{move || crate::i18n::t("Check a member’s access")}</summary><label class="settings-field"><span>{move || crate::i18n::t("Member")}</span><select prop:value=move ||preview_member.get() on:change=move |e|preview_member.set(event_target_value(&e))>{move ||editor.state.get().map(|s|s.members.into_iter().map(|m|view!{<option value=m.account_id.to_string()>{m.display_name().to_owned()}</option>}).collect_view())}</select></label><button class="settings-secondary mt-3" on:click=move |_|if let (Some(s),Ok(account_id),Ok(channel_id))=(editor.state.get_untracked(),preview_member.get_untracked().parse::<AccountId>(),channel.get_untracked().parse()){editor.request(PermissionRequest::Preview{guild_id:s.guild.id,account_id,channel_id:Some(channel_id)});}>{move || crate::i18n::t("Check access")}</button><p class="settings-hint mt-3" role="status">{move ||editor.preview.get()}</p></details>
                <Show when=move ||editor.allowed(Permission::ManageChannels)><super::settings::ConfirmButton ui=ui label="Delete channel" title="Delete this channel?" detail="Its messages, attachments, pins, reactions and permission overrides are permanently deleted. This cannot be undone." target=Signal::derive(move ||editor.state.get().and_then(|s|s.channels.into_iter().find(|c|c.id.to_string()==channel.get()).map(|c|c.name)).unwrap_or_default()) action=editor.confirmed_then(move ||channel.get_untracked().parse().ok().map(|channel_id|GuildChange::DeleteChannel{channel_id}),Callback::new(move |()|channel.set(String::new())))/></Show>
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
        <section class="settings-card settings-danger-zone"><h3>{move || crate::i18n::t("Transfer ownership")}</h3><p class="settings-hint">{move || crate::i18n::t("The new owner gets full control. You remain a member and lose owner privileges.")}</p><label class="settings-field"><span>{move || crate::i18n::t("New owner")}</span><select prop:value=move ||member.get() on:change=move |e|member.set(event_target_value(&e))><option value="">{move || crate::i18n::t("Choose a member")}</option>{move ||editor.state.get().map(|s|s.members.into_iter().filter(|m|m.account_id!=s.guild.owner).map(|m|view!{<option value=m.account_id.to_string()>{m.display_name().to_owned()}</option>}).collect_view())}</select></label><super::settings::ConfirmButton ui=ui label="Transfer ownership" title="Transfer this server’s ownership?" detail="The selected member becomes the owner with full control. You stay a member but lose owner privileges. This requires recent authentication." disabled=Signal::derive(move ||member.get().is_empty()) target=Signal::derive(move ||editor.state.get().and_then(|s|s.members.into_iter().find(|m|m.account_id.to_string()==member.get()).map(|m|m.display_name().to_owned())).unwrap_or_default()) action=editor.confirmed(move ||member.get_untracked().parse().ok().map(|account_id|GuildChange::TransferOwner{account_id}))/></section>
        <section class="settings-card settings-danger-zone"><h3>{move || crate::i18n::t("Delete server")}</h3><p class="settings-hint">{move || crate::i18n::t("Permanently deletes this server, its channels and all messages. This cannot be undone.")}</p><p class="settings-hint">{move ||editor.state.get().map(|s|thiscord_frontend::localized_format!(crate::i18n::language(), "Type {} to confirm.",s.guild.name))}</p><Field label="Server name" value=confirmation/><super::settings::ConfirmButton ui=ui label="Delete server" title="Permanently delete this server?" detail="All channels, messages, attachments, roles, invites, soundboard sounds, bots and memberships will be removed. This cannot be undone." disabled=Signal::derive(move ||!confirmed()) target=Signal::derive(move ||editor.state.get().map(|s|s.guild.name).unwrap_or_default()) action=editor.confirmed(||Some(GuildChange::Delete{}))/></section>
    </Show>}
}

fn instance_role_name(role: InstanceRole) -> &'static str {
    match role {
        InstanceRole::Owner => crate::i18n::t("Instance owner"),
        InstanceRole::Admin => crate::i18n::t("Administrator"),
        InstanceRole::User => crate::i18n::t("Free"),
    }
}

pub(super) fn allocation_summary(access: &InstanceAccess) -> String {
    access
        .policies
        .iter()
        .find(|p| Some(p.id) == access.policy_id)
        .map(|p| {
            thiscord_frontend::localized_format!(
                crate::i18n::language(),
                "{} · Owned servers: {} / {} · Per server: {} text, {} voice, {} members, {} roles",
                policy_name(p),
                access.owned_guilds,
                p.limits.owned_guilds,
                p.limits.text_channels,
                p.limits.voice_channels,
                p.limits.members,
                p.limits.roles
            )
        })
        .unwrap_or_else(|| instance_role_name(access.role).into())
}

#[component]
fn CapacitySummary(editor: Editor) -> impl IntoView {
    view! {<section class="settings-card"><h3>{move ||crate::i18n::t("Server limits")}</h3>
        {move ||editor.state.get().and_then(|s|s.capacity.map(|c|{
            let counts = [("Text channels",s.channels.iter().filter(|ch|ch.kind==ChannelKind::Text).count(),c.limits.text_channels),
                ("Voice channels",s.channels.iter().filter(|ch|ch.kind==ChannelKind::Voice).count(),c.limits.voice_channels),
                ("Members",s.members.len(),c.limits.members),("Roles",s.roles.len(),c.limits.roles)];
            let role_name = if c.builtin { instance_role_name(c.owner_role).to_owned() } else { c.role_name };
            view!{<p class="settings-hint">{thiscord_frontend::localized_format!(crate::i18n::language(),"Owner's instance role: {}",role_name)}</p>
                {counts.into_iter().map(|(label,used,max)|view!{<div class="settings-info-row"><span>{crate::i18n::t(label)}</span><strong class:text-amber-300={used>=max as usize}>{format!("{used} / {max}")}</strong></div>}).collect_view()}}
        }))}
        <p class="settings-hint">{move ||crate::i18n::t("Limits follow the server owner. Members include the owner; roles include Everyone. Existing content is preserved when limits decrease. At or above a limit, remove items before adding more.")}</p>
        <button class="settings-secondary" disabled=move ||editor.ui.busy.get()||editor.ui.settings_dirty.get() on:click=move |_|if let Some(s)=editor.state.get_untracked(){editor.request(PermissionRequest::Inspect{guild_id:s.guild.id});}>{move ||crate::i18n::t("Refresh limits")}</button>
    </section>}
}

fn policy_name(policy: &InstanceRolePolicy) -> String {
    if policy.builtin {
        instance_role_name(policy.role).into()
    } else {
        policy.name.clone()
    }
}

fn authority_name(role: InstanceRole) -> &'static str {
    match role {
        InstanceRole::User => crate::i18n::t("Standard user"),
        _ => instance_role_name(role),
    }
}

#[component]
fn InstancePolicies(
    editor: Editor,
    dirty: RwSignal<bool>,
    blocked: Signal<bool>,
    select_assignee: Callback<AccountId>,
) -> impl IntoView {
    let ui = editor.ui;
    let selected = editor.selected_instance_role;
    selected.set(
        editor
            .access
            .get_untracked()
            .and_then(|a| a.policy_id)
            .map(|id| id.to_string())
            .unwrap_or_default(),
    );
    let name = RwSignal::new(String::new());
    let authority = RwSignal::new(InstanceRole::User);
    let owned = RwSignal::new(String::new());
    let text = RwSignal::new(String::new());
    let voice = RwSignal::new(String::new());
    let members = RwSignal::new(String::new());
    let roles = RwSignal::new(String::new());
    let owner = move || {
        editor
            .access
            .get()
            .is_some_and(|a| a.role == InstanceRole::Owner)
    };
    let at_capacity = move || editor.access.get().is_some_and(|a| a.policies.len() >= 32);
    let policy = Memo::new(move |_| {
        editor.access.get().and_then(|a| {
            a.policies
                .into_iter()
                .find(|p| p.id.to_string() == selected.get())
        })
    });
    let defaults = RoleLimits {
        owned_guilds: 1,
        text_channels: 2,
        voice_channels: 2,
        members: 25,
        roles: 10,
    };
    let reset = move || {
        let p = policy.get_untracked();
        let limits = p.as_ref().map(|p| p.limits).unwrap_or(defaults);
        name.set(p.as_ref().map(|p| p.name.clone()).unwrap_or_default());
        authority.set(p.map(|p| p.role).unwrap_or(InstanceRole::User));
        owned.set(limits.owned_guilds.to_string());
        text.set(limits.text_channels.to_string());
        voice.set(limits.voice_channels.to_string());
        members.set(limits.members.to_string());
        roles.set(limits.roles.to_string());
    };
    let entered = move || {
        Some(RoleLimits {
            owned_guilds: owned.get().parse().ok()?,
            text_channels: text.get().parse().ok()?,
            voice_channels: voice.get().parse().ok()?,
            members: members.get().parse().ok()?,
            roles: roles.get().parse().ok()?,
        })
    };
    let changed = move || {
        let p = policy.get();
        name.get() != p.as_ref().map(|p| p.name.clone()).unwrap_or_default()
            || authority.get() != p.as_ref().map(|p| p.role).unwrap_or(InstanceRole::User)
            || Some(p.map(|p| p.limits).unwrap_or(defaults)) != entered()
    };
    // Track the response itself so even an unchanged policy resets a rejected edit.
    Effect::new(move |_| {
        editor.access.track();
        selected.track();
        editor.instance_members.set(None);
        if !selected.get_untracked().is_empty() && policy.get_untracked().is_none() {
            selected.set(
                editor
                    .access
                    .get_untracked()
                    .and_then(|a| a.policy_id)
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            );
        }
        reset();
    });
    Effect::new(move |_| dirty.set(changed()));
    on_cleanup(move || dirty.set(false));
    let valid = move || {
        (1..=80).contains(&name.get().trim().chars().count())
            && entered().is_some_and(RoleLimits::valid)
            && (policy.get().is_some() || !at_capacity())
    };
    view! {<form class="settings-card" on:submit=move |e:leptos::ev::SubmitEvent|{
        e.prevent_default();
        if !owner() || blocked.get() || !valid() || !changed() { return; }
        if let Some(limits) = entered() {
            let command = if let Some(p) = policy.get_untracked() {
                PermissionRequest::SetInstanceRoleLimits { role_id: p.id, name: name.get_untracked().trim().into(), revision: p.revision, limits }
            } else {
                PermissionRequest::CreateInstanceRole { name: name.get_untracked().trim().into(), role: authority.get_untracked(), limits }
            };
            editor.request(command);
        }
    }><h3>{move ||crate::i18n::t("Instance roles and limits")}</h3>
        <p class="settings-hint">{move ||crate::i18n::t("Only the instance owner can manage roles after confirming their identity. Administrators can inspect policies. Everyone can view their own allowance. Instance roles grant no access to other people's servers.")}</p>
        <label class="settings-field"><span>{move ||crate::i18n::t("Instance role")}</span><select disabled=move ||ui.busy.get()||changed()||blocked.get() prop:value=move ||selected.get() on:change=move |e|selected.set(event_target_value(&e))>
            {move ||editor.access.get().map(|a|a.policies.into_iter().map(|p|view!{<option value=p.id.to_string()>{policy_name(&p)}</option>}).collect_view())}
            <Show when=owner><option value="" disabled=at_capacity>{move ||crate::i18n::t("＋ New role")}</option></Show>
        </select></label>
        <p class="settings-hint">{move ||editor.access.get().map(|a|thiscord_frontend::localized_format!(crate::i18n::language(),"Role definitions: {} / 32",a.policies.len()))}</p>
        <fieldset class="space-y-4" disabled=move ||ui.busy.get()||!owner()||blocked.get()>
            <fieldset disabled=move ||policy.get().is_some_and(|p|p.builtin)><Field label="Role name (1–80 characters)" value=name/></fieldset>
            <label class="settings-field"><span>{move ||crate::i18n::t("Access level")}</span><select disabled=move ||policy.get().is_some() prop:value=move ||match authority.get(){InstanceRole::Owner=>"owner",InstanceRole::Admin=>"admin",InstanceRole::User=>"user"} on:change=move |e|authority.set(if event_target_value(&e)=="admin"{InstanceRole::Admin}else{InstanceRole::User})>
                <option value="user">{move ||authority_name(InstanceRole::User)}</option><option value="admin">{move ||authority_name(InstanceRole::Admin)}</option>
                <Show when=move ||policy.get().is_some_and(|p|p.role==InstanceRole::Owner)><option value="owner">{move ||authority_name(InstanceRole::Owner)}</option></Show>
            </select></label>
            <p class="settings-hint">{move ||crate::i18n::t("Access level cannot change after creation. Built-in role names cannot change. Administrator roles can inspect instance policies; only the owner can manage them.")}</p>
            <Field label="Owned servers (1–100)" value=owned kind="number"/>
            <Field label="Text channels per server (1–100)" value=text kind="number"/>
            <Field label="Voice channels per server (1–100)" value=voice kind="number"/>
            <Field label="Members per server (1–500)" value=members kind="number"/>
            <Field label="Roles per server, including Everyone (1–100)" value=roles kind="number"/>
        </fieldset>
        <p class="settings-hint">{move ||crate::i18n::t("Lowering a limit preserves existing servers and content, but blocks further growth in that category. Ownership transfers must fit the new owner's limits. The installation also has a shared capacity of 100 servers.")}</p>
        <Show when=move ||changed()><p class="settings-hint">{move ||crate::i18n::t("Unsaved changes. Save or reset before selecting another role.")}</p></Show>
        <Show when=owner><div class="settings-actions">
            <button type="button" class="settings-secondary" disabled=move ||ui.busy.get()||!changed() on:click=move |_|reset()>{move ||crate::i18n::t("Reset")}</button>
            <button class="settings-primary" disabled=move ||ui.busy.get()||blocked.get()||!changed()||!valid()>{move ||if policy.get().is_some(){crate::i18n::t("Save role")}else{crate::i18n::t("Create role")}}</button>
        </div>
        <Show when=move ||policy.get().is_some_and(|p|!p.builtin)>
            <p class="settings-hint">{move ||crate::i18n::t("Only unused custom roles can be deleted. Assign every account using this role to another role first.")}</p>
            <super::settings::ConfirmButton ui=ui label="Delete role" title="Delete this instance role?" detail="Only custom roles with no assigned accounts can be deleted. This cannot be undone." disabled=Signal::derive(move ||changed()||blocked.get()) target=Signal::derive(move ||policy.get().map(|p|p.name).unwrap_or_default()) action=editor.confirmed_request(move ||owner().then(||policy.get_untracked().filter(|p|!p.builtin)).flatten().map(|p|PermissionRequest::DeleteInstanceRole{role_id:p.id,revision:p.revision}))/>
        </Show>
        <Show when=move ||policy.get().is_some()>
            <InstanceAssignees editor=editor blocked=Signal::derive(move ||changed()||blocked.get()) select_assignee=select_assignee/>
        </Show></Show>
    </form>}
}

#[component]
fn InstanceAssignees(
    editor: Editor,
    blocked: Signal<bool>,
    select_assignee: Callback<AccountId>,
) -> impl IntoView {
    let page = Signal::derive(move || {
        editor
            .instance_members
            .get()
            .filter(|p| p.role_id.to_string() == editor.selected_instance_role.get())
    });
    let load = move |page| {
        if blocked.get_untracked()
            || !editor
                .access
                .get_untracked()
                .is_some_and(|a| a.role == InstanceRole::Owner)
        {
            return;
        }
        if let Ok(role_id) = editor.selected_instance_role.get_untracked().parse() {
            editor.request(PermissionRequest::ListInstanceRoleMembers { role_id, page });
        }
    };
    view! {<section class="space-y-4">
        <h3>{move ||crate::i18n::t("Accounts using this role")}</h3>
        <p class="settings-hint">{move ||crate::i18n::t("Includes automatic built-in assignments. Shows up to 50 accounts per page. Reassign an account using the assignment form below.")}</p>
        <fieldset class="space-y-4" disabled=move ||editor.ui.busy.get()||blocked.get()>
            <div class="settings-actions">
                <button type="button" class="settings-secondary" on:click=move |_|load(PageRequest::default())>{move ||crate::i18n::t("First page")}</button>
                <button type="button" class="settings-secondary" on:click=move |_|load(page.get_untracked().map(|p|p.request).unwrap_or_default())>{move ||crate::i18n::t("Refresh assignees")}</button>
                <button type="button" class="settings-secondary" disabled=move ||page.get().is_none_or(|p|p.page.next_cursor.is_none()) on:click=move |_|if let Some(next)=page.get_untracked().and_then(|p|p.page.next_cursor.clone()){load(PageRequest{after:Some(next),..PageRequest::default()});}>{move ||crate::i18n::t("Next page")}</button>
            </div>
            {move ||match page.get() {
                None => view!{<p class="settings-hint">{crate::i18n::t("Load or refresh assignees to inspect this role. Role and assignment changes clear this list.")}</p>}.into_any(),
                Some(p) if p.page.items.is_empty() => view!{<p class="settings-hint">{crate::i18n::t("No accounts on this page. Return to the first page to check this role again.")}</p>}.into_any(),
                Some(p) => p.page.items.into_iter().map(|member| {
                    let account_id = member.account_id;
                    view!{<div class="settings-info-row"><div><strong>{member.username}</strong><p class="settings-hint break-all">{account_id.to_string()}</p></div>
                        <button type="button" class="settings-secondary" disabled=move ||editor.access.get().is_none_or(|a|a.owner==Some(account_id)) on:click=move |_|select_assignee.run(account_id)>{move ||crate::i18n::t("Reassign in form below")}</button>
                    </div>}
                }).collect_view().into_any(),
            }}
        </fieldset>
    </section>}
}

#[component]
pub(super) fn InstanceSettings(ui: Ui) -> impl IntoView {
    let editor = editor(ui);
    let target = RwSignal::new(String::new());
    let assigned_role = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let policy_dirty = RwSignal::new(false);
    let assignment_dirty = Signal::derive(move || {
        !target.get().is_empty()
            || !assigned_role.get().is_empty()
            || !confirmation.get().is_empty()
    });
    let reset_assignment = move || {
        target.set(String::new());
        assigned_role.set(String::new());
        confirmation.set(String::new());
    };
    Effect::new(move |_| {
        editor.access.track();
        reset_assignment();
    });
    Effect::new(move |_| {
        ui.settings_dirty
            .set(policy_dirty.get() || assignment_dirty.get())
    });
    on_cleanup(move || ui.settings_dirty.set(false));
    let owner = move || {
        editor
            .access
            .get()
            .is_some_and(|a| a.role == InstanceRole::Owner)
    };
    let assignable = move || {
        let Ok(account_id) = target.get().trim().parse::<AccountId>() else {
            return false;
        };
        editor.access.get().is_some_and(|a| {
            a.owner != Some(account_id)
                && a.policies.iter().any(|p| {
                    p.id.to_string() == assigned_role.get() && p.role != InstanceRole::Owner
                })
        })
    };
    editor.request(PermissionRequest::Instance {});
    view! {
        <section class="settings-card"><h3>{move ||crate::i18n::t("Instance access")}</h3><p class="settings-hint">{move ||editor.access.get().map(|a|allocation_summary(&a))}</p>
            <Show when=move ||editor.access.get().is_none()><p class="settings-hint">{move ||if ui.busy.get(){crate::i18n::t("Loading instance roles…")}else{crate::i18n::t("Instance roles are unavailable. Refresh to try again.")}}</p></Show>
            <button class="settings-secondary" disabled=move ||ui.busy.get()||ui.settings_dirty.get() on:click=move |_|editor.request(PermissionRequest::Instance{})>{move ||crate::i18n::t("Refresh limits")}</button>
            <Show when=owner><p class="settings-hint">{move ||crate::i18n::t("Confirm your identity before creating, editing, assigning or deleting instance roles.")}</p><button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|super::settings::navigate(ui,"reauthenticate")>{move ||crate::i18n::t("Confirm identity")}</button></Show>
        </section>
        <Show when=move ||editor.access.get().is_some_and(|a|a.role!=InstanceRole::User)><InstancePolicies editor=editor dirty=policy_dirty blocked=assignment_dirty select_assignee=Callback::new(move |account_id:AccountId|{
            if owner() && !policy_dirty.get_untracked() && !assignment_dirty.get_untracked() && !ui.busy.get_untracked() {
                reset_assignment();
                target.set(account_id.to_string());
            }
        })/></Show>
        <Show when=owner><section class="settings-card"><h3>{move ||crate::i18n::t("Assign instance role")}</h3>
            <fieldset class="space-y-4" disabled=move ||ui.busy.get()||policy_dirty.get()>
                <Field label="Verified account ID" value=target/>
                <label class="settings-field"><span>{move ||crate::i18n::t("Instance role")}</span><select prop:value=move ||assigned_role.get() on:change=move |e|assigned_role.set(event_target_value(&e))><option value="">{move ||crate::i18n::t("Choose a role")}</option>
                    {move ||editor.access.get().map(|a|a.policies.into_iter().filter(|p|p.role!=InstanceRole::Owner).map(|p|view!{<option value=p.id.to_string()>{format!("{} · {}",policy_name(&p),authority_name(p.role))}</option>}).collect_view())}
                </select></label>
                <p class="settings-hint">{move ||crate::i18n::t("Assignment replaces the account's current instance role. The instance owner's role can only change through ownership transfer.")}</p>
                <div class="settings-actions"><button class="settings-secondary" disabled=move ||!assignable() on:click=move |_|if owner() && assignable() && let (Ok(account_id),Ok(role_id))=(target.get_untracked().trim().parse::<AccountId>(),assigned_role.get_untracked().parse::<InstanceRoleId>()){editor.request(PermissionRequest::SetInstanceRole{account_id,role_id});}>{move ||crate::i18n::t("Assign role")}</button>
                <button class="settings-secondary" disabled=move ||!assignment_dirty.get() on:click=move |_|reset_assignment()>{move ||crate::i18n::t("Reset")}</button></div>
                <details class="settings-danger-zone"><summary>{move ||crate::i18n::t("Transfer instance ownership")}</summary><p class="settings-hint">{move ||crate::i18n::t("Requires recent authentication. You will become a regular instance member. Type TRANSFER INSTANCE to continue.")}</p><Field label="Confirmation" value=confirmation/>
                    <super::settings::ConfirmButton ui=ui label="Transfer instance" title="Transfer instance ownership?" detail="The selected account becomes the instance owner. You lose owner privileges." disabled=Signal::derive(move ||policy_dirty.get()||confirmation.get()!="TRANSFER INSTANCE"||target.get().trim().parse::<AccountId>().ok().is_none_or(|id|editor.access.get().is_none_or(|a|a.owner==Some(id)))) target=Signal::derive(move ||target.get().trim().to_owned()) action=editor.confirmed_request(move ||owner().then(||target.get_untracked().trim().parse().ok()).flatten().map(|account_id|PermissionRequest::TransferInstance{account_id}))/>
                </details>
            </fieldset>
        </section></Show>
    }
}
