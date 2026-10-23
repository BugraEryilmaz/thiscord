use super::super::{Field, Ui};
use leptos::prelude::*;
use thiscord_shared::{CategoryId, ChannelId, GuildId, permissions::*};

#[derive(Clone, Copy)]
struct Panel {
    ui: Ui,
    guild: GuildId,
    revision: RwSignal<i64>,
    channels: RwSignal<Vec<Channel>>,
    categories: RwSignal<Vec<Category>>,
    invites: RwSignal<Vec<GuildInvite>>,
    link: RwSignal<String>,
    message: RwSignal<String>,
    loaded: RwSignal<bool>,
    invite_mode: bool,
}
impl Panel {
    fn new(ui: Ui, guild: GuildId, invite_mode: bool) -> Self {
        Self {
            ui,
            guild,
            revision: RwSignal::new(0),
            channels: RwSignal::new(vec![]),
            categories: RwSignal::new(vec![]),
            invites: RwSignal::new(vec![]),
            link: RwSignal::new(String::new()),
            message: RwSignal::new(String::new()),
            loaded: RwSignal::new(false),
            invite_mode,
        }
    }
    async fn load(self) {
        let command = if self.invite_mode {
            PermissionRequest::ListInvites {
                guild_id: self.guild,
            }
        } else {
            PermissionRequest::InspectOrganization {
                guild_id: self.guild,
            }
        };
        let result = super::super::servers::request(self.ui, command).await;
        if self.loaded.is_disposed() {
            return;
        }
        match result {
            Ok(PermissionResponse::Organization {
                guild,
                channels,
                categories,
            }) => {
                self.revision.set(guild.revision);
                self.channels.set(channels);
                self.categories.set(categories);
                self.loaded.set(true);
            }
            Ok(PermissionResponse::Invites { guild, invites }) => {
                self.revision.set(guild.revision);
                self.invites.set(invites);
                self.loaded.set(true);
            }
            Err(e) => {
                self.loaded.set(false);
                self.channels.set(vec![]);
                self.categories.set(vec![]);
                self.invites.set(vec![]);
                self.link.set(String::new());
                self.message.set(e);
            }
            _ => self.message.set("Unexpected server response".into()),
        }
    }
    fn request(self, command: PermissionRequest) {
        if self.ui.busy.get_untracked() || !self.loaded.get_untracked() {
            return;
        }
        self.ui.busy.set(true);
        let revoking = matches!(command, PermissionRequest::RevokeInvite { .. });
        leptos::task::spawn_local(async move {
            let result = super::super::servers::request(self.ui, command).await;
            if !self.loaded.is_disposed() {
                match result {
                    Ok(PermissionResponse::InviteCreated { code, .. }) => {
                        let base = if crate::account_client::api_base().is_empty() {
                            window().location().origin().unwrap_or_default()
                        } else {
                            crate::account_client::api_base().to_owned()
                        };
                        self.link.set(format!("{base}/invite#{code}"));
                        self.message.set(
                            "Invite created. Copy this link now; the code is shown only once."
                                .into(),
                        );
                    }
                    Ok(_) => {
                        if revoking {
                            self.link.set(String::new());
                        }
                        self.message.set("Saved.".into());
                    }
                    Err(e) => self.message.set(thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "{error}. Refreshed; review before trying again.",
                        error = crate::i18n::message(e)
                    )),
                }
                self.load().await;
                super::super::servers::refresh(self.ui).await;
            }
            self.ui.busy.set(false);
        });
    }
    fn change(self, change: GuildChange) {
        self.request(PermissionRequest::Change {
            guild_id: self.guild,
            revision: self.revision.get_untracked(),
            change,
        });
    }
    fn number<T: std::str::FromStr>(self, value: String, label: &str) -> Option<T> {
        match value.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                self.message.set(thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "Enter a whole number for {label}.",
                    label = crate::i18n::t(label)
                ));
                None
            }
        }
    }
}

#[component]
pub(crate) fn OrganizationPanel(ui: Ui) -> impl IntoView {
    let guild = ui.server.get_untracked().map(|s| s.guild.id);
    view! { {guild.map(|guild|view!{<Organization ui=ui guild=guild/>})} }
}
#[component]
fn Organization(ui: Ui, guild: GuildId) -> impl IntoView {
    let panel = Panel::new(ui, guild, false);
    let channel = RwSignal::new(None::<ChannelId>);
    let category = RwSignal::new(None::<CategoryId>);
    let name = RwSignal::new(String::new());
    let position = RwSignal::new(String::from("0"));
    let placement = RwSignal::new(None::<CategoryId>);
    let category_name = RwSignal::new(String::new());
    let category_position = RwSignal::new(String::from("0"));
    let new_category = RwSignal::new(String::new());
    let new_channel = RwSignal::new(String::new());
    let voice = RwSignal::new(false);
    let selected = move || {
        panel
            .channels
            .get()
            .into_iter()
            .find(|c| Some(c.id) == channel.get())
    };
    Effect::new(move |_| {
        if let Some(c) = selected() {
            name.set(c.name);
            position.set(c.position.to_string());
            placement.set(c.category_id);
        }
    });
    Effect::new(move |_| {
        if let Some(c) = panel
            .categories
            .get()
            .into_iter()
            .find(|c| Some(c.id) == category.get())
        {
            category_name.set(c.name);
            category_position.set(c.position.to_string());
        }
    });
    leptos::task::spawn_local(async move {
        panel.load().await;
    });
    view! {
        <p class="settings-hint">{move || crate::i18n::t("Categories organize channels only. Each channel keeps its existing permissions and history. Lower order numbers appear first; ties use channel IDs.")}</p>
        <p role="status" class="settings-hint">{move ||crate::i18n::message(panel.message.get())}</p>
        <button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|leptos::task::spawn_local(async move {panel.load().await;})>{move || crate::i18n::t("Refresh")}</button>
        <fieldset class="space-y-5" disabled=move ||ui.busy.get() || !panel.loaded.get()>
            <section class="settings-card"><h3>{move || crate::i18n::t("Channels")}</h3>
                <label class="settings-field"><span>{move || crate::i18n::t("Channel")}</span><select prop:value=move ||channel.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |ev|channel.set(event_target_value(&ev).parse().ok())><option value="">{move || crate::i18n::t("Choose a channel")}</option>{move ||panel.channels.get().into_iter().map(|c|view!{<option value=c.id.to_string()>{format!("{} {}",if c.kind==ChannelKind::Voice{crate::i18n::t("Voice")}else{"#"},c.name)}</option>}).collect_view()}</select></label>
                <Show when=move ||selected().is_some()>
                    <Field label="Channel name" value=name/><Field label="Order (0–10000)" value=position kind="number"/>
                    <label class="settings-field"><span>{move || crate::i18n::t("Category")}</span><select prop:value=move ||placement.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |ev|placement.set(event_target_value(&ev).parse().ok())><option value="">{move || crate::i18n::t("No category")}</option>{move ||panel.categories.get().into_iter().map(|c|view!{<option value=c.id.to_string()>{c.name}</option>}).collect_view()}</select></label>
                    <super::super::settings::ConfirmButton ui=ui label="Save channel" title="Save channel name and placement?" detail="Choosing No category removes this channel from its category. Permissions and history are preserved." on_confirm=Callback::new(move |()|if let (Some(channel_id),Some(position))=(channel.get_untracked(),panel.number(position.get_untracked(),"channel order")){panel.change(GuildChange::EditChannel {channel_id,name:name.get_untracked(),category_id:placement.get_untracked(),position});})/>
                    <super::super::settings::ConfirmButton ui=ui label="Delete channel" title="Delete this channel?" detail="Permanently deletes this channel and its messages." on_confirm=Callback::new(move |()|if let Some(channel_id)=channel.get_untracked(){panel.change(GuildChange::DeleteChannel {channel_id});channel.set(None);})/>
                </Show>
                <details><summary>{move || crate::i18n::t("Create channel")}</summary><Field label="Channel name" value=new_channel/><label class="settings-field"><span>{move || crate::i18n::t("Type")}</span><select on:change=move |e|voice.set(event_target_value(&e)=="voice")><option value="text">{move || crate::i18n::t("Text")}</option><option value="voice">{move || crate::i18n::t("Voice")}</option></select></label><button class="settings-primary" on:click=move |_|panel.change(GuildChange::CreateChannel {name:new_channel.get_untracked(),kind:if voice.get_untracked(){ChannelKind::Voice}else{ChannelKind::Text}})>{move || crate::i18n::t("Create channel")}</button></details>
            </section>
            <section class="settings-card"><h3>{move || crate::i18n::t("Categories")}</h3>
                <label class="settings-field"><span>{move || crate::i18n::t("Category")}</span><select prop:value=move ||category.get().map(|id|id.to_string()).unwrap_or_default() on:change=move |ev|category.set(event_target_value(&ev).parse().ok())><option value="">{move || crate::i18n::t("Choose a category")}</option>{move ||panel.categories.get().into_iter().map(|c|view!{<option value=c.id.to_string()>{c.name}</option>}).collect_view()}</select></label>
                <Show when=move ||category.get().is_some()><Field label="Category name" value=category_name/><Field label="Order (0–10000)" value=category_position kind="number"/>
                    <button class="settings-primary" on:click=move |_|if let (Some(category_id),Some(position))=(category.get_untracked(),panel.number(category_position.get_untracked(),"category order")){panel.change(GuildChange::EditCategory {category_id,name:category_name.get_untracked(),position});}>{move || crate::i18n::t("Save category")}</button>
                    <super::super::settings::ConfirmButton ui=ui label="Delete category" title="Delete this category?" detail="Its channels move to No category. Channels, messages and permissions are preserved." on_confirm=Callback::new(move |()|if let Some(category_id)=category.get_untracked(){panel.change(GuildChange::DeleteCategory {category_id});category.set(None);})/>
                </Show>
                <Field label="New category name" value=new_category/><button class="settings-primary" on:click=move |_|panel.change(GuildChange::CreateCategory {name:new_category.get_untracked()})>{move || crate::i18n::t("Create category")}</button>
            </section>
        </fieldset>
    }
}
#[component]
pub(crate) fn InvitesPanel(ui: Ui) -> impl IntoView {
    let guild = ui.server.get_untracked().map(|s| s.guild.id);
    view! { {guild.map(|guild|view!{<Invites ui=ui guild=guild/>})} }
}
#[component]
fn Invites(ui: Ui, guild: GuildId) -> impl IntoView {
    let panel = Panel::new(ui, guild, true);
    let duration = RwSignal::new(86400u32);
    let uses = RwSignal::new(String::from("10"));
    leptos::task::spawn_local(async move {
        panel.load().await;
    });
    view! {
        <p class="settings-hint">{move || crate::i18n::t("Anyone with an invite may join without the server password, after signing in and verifying their email. Bans and member limits still apply. Share links only with intended recipients.")}</p>
        <p class="settings-hint" role="status">{move ||crate::i18n::message(panel.message.get())}</p>
        <button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|leptos::task::spawn_local(async move {panel.load().await;})>{move || crate::i18n::t("Refresh invites")}</button>
        <fieldset class="settings-card" disabled=move ||ui.busy.get() || !panel.loaded.get()><h3>{move || crate::i18n::t("Create invite")}</h3>
            <label class="settings-field"><span>{move || crate::i18n::t("Expires after")}</span><select on:change=move |e|if let Ok(n)=event_target_value(&e).parse(){duration.set(n);} prop:value=move ||duration.get().to_string()><option value="3600">{move || crate::i18n::t("1 hour")}</option><option value="86400">{move || crate::i18n::t("1 day")}</option><option value="604800">{move || crate::i18n::t("7 days")}</option><option value="2419200">{move || crate::i18n::t("28 days")}</option><option value="0">{move ||crate::i18n::t("Never")}</option></select></label>
            <Field label="Maximum uses (0 = unlimited, 1–500)" value=uses kind="number"/>
            <button class="settings-primary" on:click=move |_|if let Some(max_uses)=panel.number(uses.get_untracked(),"maximum uses"){panel.request(PermissionRequest::CreateInvite {guild_id:guild,revision:panel.revision.get_untracked(),duration_seconds:duration.get_untracked(),max_uses});}>{move || crate::i18n::t("Create invite")}</button>
            <Show when=move ||!panel.link.get().is_empty()><label class="settings-field"><span>{move || crate::i18n::t("New invite link (shown once)")}</span><input readonly prop:value=move ||panel.link.get()/></label><button class="settings-secondary" on:click=move |_|leptos::task::spawn_local(async move {let link=panel.link.get_untracked();let result=wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(&link)).await;if !panel.message.is_disposed(){panel.message.set(if result.is_ok(){"Invite copied"}else{"Copy the invite from the field"}.into());}})>{move || crate::i18n::t("Copy link")}</button></Show>
        </fieldset>
        <section class="settings-card"><h3>{move || crate::i18n::t("Invites")}</h3><ul class="space-y-4">{move ||panel.invites.get().into_iter().map(move |invite|view!{<li class="space-y-2"><p class="text-sm">{thiscord_frontend::localized_format!(crate::i18n::language(), "Created {} · {}/{} uses · {}{}",crate::i18n::datetime(invite.created_at),invite.uses,if invite.max_uses==0{crate::i18n::t("Unlimited").to_owned()}else{invite.max_uses.to_string()},invite.expires_at.map(crate::i18n::datetime).unwrap_or_else(||crate::i18n::t("Never expires").into()),if invite.revoked{crate::i18n::t(" · revoked")}else{""})}</p><Show when=move ||!invite.revoked><super::super::settings::ConfirmButton ui=ui label="Revoke invite" title="Revoke this invite?" detail="The link will no longer allow anyone to join. Existing members stay." on_confirm=Callback::new(move |()|panel.request(PermissionRequest::RevokeInvite {guild_id:guild,revision:panel.revision.get_untracked(),invite_id:invite.id}))/></Show></li>}).collect_view()}</ul></section>
    }
}
