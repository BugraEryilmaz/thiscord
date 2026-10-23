use super::{Ui, client};
use leptos::prelude::*;
use thiscord_shared::{GuildId, bots::*};

#[derive(Clone, Copy)]
struct Manager {
    ui: Ui,
    guild: GuildId,
    bots: RwSignal<Vec<BotInfo>>,
    revision: RwSignal<i64>,
    loaded: RwSignal<bool>,
    name: RwSignal<String>,
    credential: RwSignal<String>,
    credential_name: RwSignal<String>,
    revealed: RwSignal<bool>,
    status: RwSignal<String>,
}

impl Manager {
    fn request(self, request: BotRequest) {
        if self.ui.busy.get_untracked() {
            return;
        }
        let listing = matches!(request, BotRequest::List { .. });
        if !listing && (!self.loaded.get_untracked() || !self.credential.get_untracked().is_empty())
        {
            return;
        }
        self.ui.busy.set(true);
        self.status.set(String::new());
        let session = self.ui.token.get_untracked();
        leptos::task::spawn_local(async move {
            let result =
                client::api_request::<_, BotResponse>(BOTS_PATH, &request, session.as_deref())
                    .await;
            if self.loaded.is_disposed() || self.ui.token.get_untracked() != session {
                self.ui.busy.set(false);
                return;
            }
            match result {
                Ok(BotResponse::Bots { bots, revision }) => {
                    self.bots.set(bots);
                    self.revision.set(revision);
                    self.loaded.set(true);
                }
                Ok(BotResponse::Credential {
                    bot,
                    token,
                    revision,
                }) => {
                    self.revision.set(revision);
                    self.bots.update(|bots| {
                        bots.retain(|b| b.account_id != bot.account_id);
                        bots.push(bot.clone());
                        bots.sort_by(|a, b| a.name.cmp(&b.name));
                    });
                    self.name.set(String::new());
                    self.revealed.set(false);
                    self.credential_name.set(bot.name.clone());
                    self.credential.set(token.clone());
                    let status = if client::desktop() {
                        match client::native::<()>(
                            "save_bot_credential",
                            serde_json::json!({"name":bot.name,"token":token}),
                        )
                        .await
                        {
                            Ok(()) => {
                                "Saved in your OS credential store. Run the examples on this computer using the bot name."
                            }
                            Err(_) => {
                                "Could not save the token. Copy it to your secret manager before closing, or rotate it to try again."
                            }
                        }
                    } else {
                        "Copy this token to your secret manager before closing. It is only shown once and is not saved in this browser."
                    };
                    if !self.loaded.is_disposed() && self.ui.token.get_untracked() == session {
                        self.status.set(status.into());
                    }
                }
                Ok(BotResponse::Done { revision }) => {
                    self.revision.set(revision);
                    if let BotRequest::Delete { account_id, .. } = request {
                        self.bots
                            .update(|bots| bots.retain(|bot| bot.account_id != account_id));
                    }
                    self.status
                        .set("Bot deleted. Its token and connections have been revoked.".into());
                }
                Err(error) => {
                    self.loaded.set(false);
                    self.status.set(error);
                }
            }
            if !listing && !self.loaded.is_disposed() && self.ui.token.get_untracked() == session {
                super::servers::refresh(self.ui).await;
            }
            self.ui.busy.set(false);
        });
    }
    fn locked(self) -> bool {
        self.ui.busy.get() || !self.loaded.get() || !self.credential.get().is_empty()
    }
}

#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {
        <For each=move ||{ui.server.get().filter(|s|s.can_manage_bots).map(|s|s.guild.id).into_iter().collect::<Vec<_>>()} key=|id|*id children=move |guild|view!{<GuildBots ui=ui guild=guild/>}/>
    }
}

#[component]
fn GuildBots(ui: Ui, guild: GuildId) -> impl IntoView {
    let manager = Manager {
        ui,
        guild,
        bots: RwSignal::new(vec![]),
        revision: RwSignal::new(0),
        loaded: RwSignal::new(false),
        name: RwSignal::new(String::new()),
        credential: RwSignal::new(String::new()),
        credential_name: RwSignal::new(String::new()),
        revealed: RwSignal::new(false),
        status: RwSignal::new(String::new()),
    };
    manager.request(BotRequest::List { guild_id: guild });
    let valid_name = move || {
        let name = manager.name.get();
        (3..=32).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    view! {
        <section class="settings-card">
            <h3>{move ||crate::i18n::t("Create a bot")}</h3>
            <p class="settings-hint">{move ||crate::i18n::t("Bots respond to mentions and can send messages or play audio. Create an identity here, then run a Rust bot program on your computer or server.")}</p>
            <form class="space-y-4" on:submit=move |e:leptos::ev::SubmitEvent|{
                e.prevent_default();
                if valid_name() { manager.request(BotRequest::Create { guild_id: guild, revision: manager.revision.get_untracked(), name: manager.name.get_untracked() }); }
            }>
                <label class="settings-field"><span>{move ||crate::i18n::t("Bot username")}</span><input type="text" minlength="3" maxlength="32" pattern="[a-z0-9_]{3,32}" placeholder="jukebox" autocomplete="off" spellcheck="false" required disabled=move ||manager.locked() prop:value=move ||manager.name.get() on:input=move |e|manager.name.set(event_target_value(&e))/><small>{move ||crate::i18n::t("3–32 lowercase letters, digits or underscores. This name must be unique across Thiscord.")}</small></label>
                <button class="settings-primary" disabled=move ||{manager.locked() || !valid_name() || manager.bots.get().len()>=MAX_BOTS_PER_GUILD}>{move ||crate::i18n::t("Create bot")}</button>
            </form>
        </section>
        <p class="settings-hint" role="status" aria-live="polite">{move ||crate::i18n::message(manager.status.get())}</p>
        <Show when=move ||!manager.credential.get().is_empty()>
            <section class="settings-card">
                <h3>{move ||format!("@{}",manager.credential_name.get())}</h3>
                <label class="settings-field"><span>{move ||crate::i18n::t("Bot token — keep private")}</span><input type=move ||if manager.revealed.get(){"text"}else{"password"} readonly autocomplete="off" spellcheck="false" prop:value=move ||manager.credential.get()/></label>
                <div class="settings-actions">
                    <button class="settings-secondary" on:click=move |_|manager.revealed.update(|v|*v = !*v)>{move ||crate::i18n::t(if manager.revealed.get(){"Hide token"}else{"Reveal token"})}</button>
                    <button class="settings-secondary" on:click=move |_|{
                        let token = manager.credential.get_untracked();
                        leptos::task::spawn_local(async move {
                            let result = wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(&token)).await;
                            if !manager.status.is_disposed() { manager.status.set(if result.is_ok(){"Bot token copied. Keep it private."}else{"Could not copy. Reveal the token and copy it manually."}.into()); }
                        });
                    }>{move ||crate::i18n::t("Copy token")}</button>
                    <button class="settings-primary" disabled=move ||ui.busy.get() on:click=move |_|{manager.credential.set(String::new());manager.revealed.set(false);} >{move ||crate::i18n::t("Done with token")}</button>
                </div>
                <p class="settings-hint">{move ||crate::i18n::t("This token controls the bot. After leaving this page, rotate the token if you need a replacement.")}</p>
            </section>
        </Show>
        <section class="settings-card">
            <div class="settings-actions"><h3>{move ||crate::i18n::t("Server bots")}</h3><span class="text-sm text-white/60">{move ||format!("{} / {}",manager.bots.get().len(),MAX_BOTS_PER_GUILD)}</span><button class="settings-secondary" disabled=move ||ui.busy.get() on:click=move |_|manager.request(BotRequest::List{guild_id:manager.guild})>{move ||crate::i18n::t("Refresh")}</button></div>
            <Show when=move ||!manager.loaded.get()><p class="settings-hint">{move ||crate::i18n::t(if ui.busy.get(){"Loading bots…"}else{"Refresh the bot list before making changes."})}</p></Show>
            <Show when=move ||manager.loaded.get() && manager.bots.get().is_empty()><p class="settings-hint">{move ||crate::i18n::t("No bots yet. Create your first bot above.")}</p></Show>
            <For each=move ||manager.bots.get() key=|bot|bot.account_id children=move |bot|{
                let account_id = bot.account_id;
                view!{<div class="border-t border-white/10 py-4"><strong>{format!("@{}",bot.name)}</strong>
                    <div class="settings-actions mt-3">
                        <Show when=move ||ui.server.get().is_some_and(|s|s.can_manage_roles)><button class="settings-secondary" disabled=move ||manager.locked() on:click=move |_|{ui.role_target.set(Some((guild,account_id)));super::settings::navigate(ui,"server_members");}>{move ||crate::i18n::t("Manage roles")}</button></Show>
                        <super::settings::ConfirmButton ui=ui label="Rotate token" title="Replace this bot’s token?" detail="The current token will stop working immediately and the bot will disconnect. Restart it with the new token." disabled=Signal::derive(move ||manager.locked()) on_confirm=Callback::new(move |()|manager.request(BotRequest::Rotate{guild_id:guild,revision:manager.revision.get_untracked(),account_id}))/>
                        <super::settings::ConfirmButton ui=ui label="Delete bot" title="Delete this bot?" detail="This permanently removes the bot from this server and revokes its token and active connections." disabled=Signal::derive(move ||manager.locked()) on_confirm=Callback::new(move |()|manager.request(BotRequest::Delete{guild_id:guild,revision:manager.revision.get_untracked(),account_id}))/>
                    </div>
                </div>}
            }/>
            <p class="settings-hint">{move ||crate::i18n::t("Bots inherit Everyone permissions. Use member roles and channel permissions to grant text access or Join voice and Speak. Managers must outrank a bot and cannot issue permissions they do not have.")}</p>
        </section>
        <section class="settings-card"><h3>{move ||crate::i18n::t("Run your bot")}</h3><p class="settings-hint">{move ||crate::i18n::t("Set THISCORD_BOT_SERVER and THISCORD_BOT_NAME on the bot host. Choose channels by name with THISCORD_BOT_TEXT_CHANNEL and THISCORD_BOT_VOICE_CHANNEL. The examples default to general and Music.")}</p><a class="settings-link" href="https://github.com/BugraEryilmaz/thiscord/tree/main/bots#quick-start" target="_blank" rel="noopener noreferrer">{move ||crate::i18n::t("Bot setup and examples")}</a></section>
    }
}
