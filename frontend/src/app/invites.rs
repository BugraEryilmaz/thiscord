use super::{Ui, servers};
use crate::account_client as client;
use leptos::prelude::*;
use std::collections::HashMap;
use thiscord_shared::{GuildId, InviteId, permissions::*};

#[derive(Clone, Copy)]
pub(super) struct State {
    pub pending: RwSignal<Option<String>>,
    links: RwSignal<HashMap<GuildId, (InviteId, String)>>,
    fallback: RwSignal<Option<String>>,
}
impl State {
    pub fn new() -> Self {
        Self {
            pending: RwSignal::new(None),
            links: RwSignal::new(HashMap::new()),
            fallback: RwSignal::new(None),
        }
    }
    pub fn clear_links(self) {
        self.links.update(HashMap::clear);
        self.fallback.set(None);
    }
}

pub(super) fn base() -> String {
    if client::api_base().is_empty() {
        window().location().origin().unwrap_or_default()
    } else {
        client::api_base().to_owned()
    }
}

async fn link(ui: Ui, guild_id: GuildId) -> Result<String, String> {
    // Re-check permission and revocation before reusing an in-memory capability.
    let PermissionResponse::Invites { guild, invites } =
        servers::request(ui, PermissionRequest::ListInvites { guild_id }).await?
    else {
        return Err("Unexpected server response".into());
    };
    if let Some((id, link)) = ui
        .invites
        .links
        .with_untracked(|links| links.get(&guild_id).cloned())
        && invites.iter().any(|invite| {
            invite.id == id
                && !invite.revoked
                && invite.expires_at.is_none()
                && invite.max_uses == 0
        })
    {
        return Ok(link);
    }
    let PermissionResponse::InviteCreated { invite, code, .. } = servers::request(
        ui,
        PermissionRequest::CreateInvite {
            guild_id,
            revision: guild.revision,
            duration_seconds: 0,
            max_uses: 0,
        },
    )
    .await?
    else {
        return Err("Unexpected server response".into());
    };
    let link = format!("{}/invite#{code}", base());
    ui.invites.links.update(|links| {
        links.insert(guild_id, (invite.id, link.clone()));
    });
    Ok(link)
}

pub(super) fn copy(ui: Ui, guild: GuildId) {
    if ui.busy.get_untracked() {
        return;
    }
    ui.busy.set(true);
    let token = ui.token.get_untracked();
    leptos::task::spawn_local(async move {
        let result = link(ui, guild).await;
        if token != ui.token.get_untracked() {
            return;
        }
        match result {
            Ok(link) => {
                let copied = wasm_bindgen_futures::JsFuture::from(
                    window().navigator().clipboard().write_text(&link),
                )
                .await
                .is_ok();
                if token != ui.token.get_untracked() {
                    return;
                }
                if !copied {
                    ui.invites.fallback.set(Some(link));
                }
                ui.status.set(
                    if copied {
                        "Invite link copied. Revoke it in Server Settings → Invite links."
                    } else {
                        "Copy the invite from the field"
                    }
                    .into(),
                );
                servers::refresh(ui).await;
            }
            Err(error) => ui.status.set(error),
        }
        ui.busy.set(false);
    });
}

fn receive(ui: Ui) {
    leptos::task::spawn_local(async move {
        if let Ok(Some(code)) =
            client::native::<Option<String>>("take_pending_invite", serde_json::json!({})).await
            && !ui.invites.pending.is_disposed()
        {
            ui.invites.pending.set(Some(code));
        }
    });
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let listener = StoredValue::new_local(None::<client::EventListener>);
    on_cleanup(move || listener.set_value(None));
    if client::desktop() {
        leptos::task::spawn_local(async move {
            if let Ok(handle) = client::listen("invite-pending", move |(): ()| receive(ui)).await {
                if listener.is_disposed() {
                    return;
                }
                listener.set_value(Some(handle));
                receive(ui);
            }
        });
    }
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        if let Some(dialog) = dialog.get() {
            if ui.invites.fallback.get().is_some() {
                let _ = dialog.show_modal();
            } else {
                dialog.close();
            }
        }
    });
    view! {
        <Show when=move ||ui.invites.pending.get().is_some() && ui.account.get().is_none_or(|a|!a.email_verified)>
            <div class="fixed left-1/2 top-4 z-50 w-[calc(100%-2rem)] max-w-md -translate-x-1/2 rounded-xl bg-brand p-4 text-center text-sm text-white" role="status">{move ||crate::i18n::t("Invitation ready. Sign in and verify your email to continue.")}</div>
        </Show>
        <dialog node_ref=dialog class="m-auto w-[calc(100%-2rem)] max-w-md rounded-2xl bg-surface p-6 text-white backdrop:bg-black/70" aria-label=move ||crate::i18n::t("Invite link") on:close=move |_|ui.invites.fallback.set(None)>
            <h2 class="text-xl font-semibold">{move ||crate::i18n::t("Invite link")}</h2>
            <p class="my-4 text-sm text-white/70">{move ||crate::i18n::t("Unlimited uses. Never expires. Revoke in Server Settings → Invite links.")}</p>
            <input class="w-full rounded bg-black/20 p-3" aria-label=move ||crate::i18n::t("Invite link") readonly prop:value=move ||ui.invites.fallback.get().unwrap_or_default()/>
            <div class="mt-4 flex gap-3">
                <button class="rounded bg-brand px-4 py-2" on:click=move |_| {
                    // A second, direct gesture supports browsers that expire clipboard
                    // activation while the invite request is in flight.
                    let value = ui.invites.fallback.get_untracked().unwrap_or_default();
                    let promise = window().navigator().clipboard().write_text(&value);
                    leptos::task::spawn_local(async move {
                        if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                            ui.invites.fallback.set(None);
                            ui.status.set("Invite copied".into());
                        }
                    });
                }>{move ||crate::i18n::t("Copy link")}</button>
                <button class="rounded bg-white/10 px-4 py-2" on:click=move |_|ui.invites.fallback.set(None)>{move ||crate::i18n::t("Close")}</button>
            </div>
        </dialog>
    }
}
