use super::Ui;
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::update::{UpdatePhase, UpdateStatus};

fn busy(ui: Ui) -> bool {
    matches!(
        ui.updates.get().phase,
        UpdatePhase::Checking | UpdatePhase::Downloading | UpdatePhase::Installing
    )
}
fn request(ui: Ui, command: &'static str) {
    ui.update_error.set(String::new());
    leptos::task::spawn_local(async move {
        if let Err(error) = native::<()>(command, json!({})).await {
            ui.update_error.set(error);
        }
    });
}

#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let listener = StoredValue::new_local(None::<crate::account_client::EventListener>);
    on_cleanup(move || listener.set_value(None));
    if desktop() {
        leptos::task::spawn_local(async move {
            if let Ok(handle) =
                crate::account_client::listen("update-status", move |status: UpdateStatus| {
                    if !ui.updates.is_disposed() {
                        ui.updates.set(status);
                    }
                })
                .await
            {
                if listener.is_disposed() {
                    return;
                }
                listener.set_value(Some(handle));
                if let Ok(status) = native::<UpdateStatus>("update_status", json!({})).await
                    && !ui.updates.is_disposed()
                {
                    ui.updates.set(status);
                }
            }
        });
    }
    view! {<Show when=desktop>
        <Show when=move||ui.account.get().is_none()>
            <button class="fixed bottom-4 right-4 z-30 rounded-lg bg-surface px-3 py-2 text-sm text-white/70 hover:text-white" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}>{move || crate::i18n::t("Updates")}</button>
        </Show>
        <dialog node_ref=dialog aria-label=move || crate::i18n::t("Application updates") class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-lg overflow-y-auto rounded-xl border border-white/20 bg-surface p-6 text-white backdrop:bg-black/70">
            <button class="float-right rounded px-2 py-1 hover:bg-white/10" aria-label=move || crate::i18n::t("Close updates") on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>{move || crate::i18n::t("Close")}</button>
            <Panel ui=ui/>
        </dialog>
    </Show>}
}

#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {<div class="settings-card">
        <h2 class="settings-panel-title text-xl font-semibold">{move || crate::i18n::t("Application updates")}</h2>
        <Show when=desktop fallback=||view!{<p>{move || crate::i18n::t("Refresh the browser to load the latest web version.")}</p>}>
            <p>{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Installed version: {}",ui.updates.get().current_version)}</p>
            <p class="text-sm text-white/60">{move || crate::i18n::t("New releases arrive through your notification subscription. You can also check manually. Updates install only when you choose Install and restart.")}</p>
            <p role="status">{move||crate::i18n::message(ui.updates.get().message)}</p>
            <Show when=move||ui.updates.get().phase==UpdatePhase::Downloading>
                <progress class="w-full" max="100" value=move||{let s=ui.updates.get();s.total.filter(|n|*n>0).map(|n|(100.0*s.downloaded as f64/n as f64).min(100.0))}/>
                <p class="text-sm">{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Downloaded {} MiB",crate::i18n::number(ui.updates.get().downloaded as f64/(1024.0*1024.0),1))}</p>
            </Show>
            <div class="flex flex-wrap gap-3">
                <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-50" disabled=move||busy(ui)||ui.updates.get().phase==UpdatePhase::Unsupported on:click=move |_|request(ui,"update_check")>{move || crate::i18n::t("Check for updates")}</button>
                <Show when=move||ui.updates.get().available_version.is_some()>
                    <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||busy(ui)||ui.voice.get().channel_id.is_some() on:click=move |_|request(ui,"update_install")>{move || crate::i18n::t("Install and restart")}</button>
                </Show>
            </div>
            <Show when=move||ui.voice.get().channel_id.is_some()><p class="text-sm text-white/60">{move || crate::i18n::t("Disconnect from voice before installing.")}</p></Show>
            <p role="alert" class="text-sm text-red-300">{move||crate::i18n::message(ui.update_error.get())}</p>
        </Show>
    </div>}
}
