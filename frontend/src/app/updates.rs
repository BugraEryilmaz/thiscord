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
        <Prompt ui=ui/>
        <Show when=move||ui.account.get().is_none()>
            <button class="fixed bottom-4 right-4 z-30 rounded-lg bg-surface px-3 py-2 text-sm text-white/70 hover:text-white" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}>{move || crate::i18n::t("Updates")}</button>
        </Show>
        <dialog node_ref=dialog aria-label=move || crate::i18n::t("Application updates") class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-lg overflow-y-auto rounded-xl border border-white/20 bg-surface p-6 text-white backdrop:bg-black/70">
            <button class="float-right rounded px-2 py-1 hover:bg-white/10" aria-label=move || crate::i18n::t("Close updates") on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>{move || crate::i18n::t("Close")}</button>
            <Panel ui=ui/>
        </dialog>
    </Show>}
}

/// Startup and announced updates open this prompt; dismissal lasts for the session.
#[component]
fn Prompt(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let dismissed = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let status = ui.updates.get();
        let Some(d) = dialog.get() else { return };
        if status.phase == UpdatePhase::Available
            && status.available_version.is_some()
            && status.available_version != dismissed.get()
            && !d.open()
        {
            let _ = d.show_modal();
        }
    });
    let installing = move || ui.updates.get().phase == UpdatePhase::Installing;
    let later = move || {
        if installing() {
            return;
        }
        dismissed.set(ui.updates.get_untracked().available_version);
        if let Some(d) = dialog.get_untracked() {
            d.close();
        }
    };
    let downloading = move || {
        matches!(
            ui.updates.get().phase,
            UpdatePhase::Downloading | UpdatePhase::Installing
        )
    };
    view! {
        <dialog node_ref=dialog class="update-dialog" aria-labelledby="update-dialog-title" on:cancel=move |e: web_sys::Event|{e.prevent_default();later();}>
            <div class="flex h-full flex-col items-center justify-center gap-5 overflow-y-auto px-8 py-10 text-center">
                <div class="flex h-20 w-20 items-center justify-center rounded-full bg-brand/20 text-brand" aria-hidden="true">
                    <svg class="h-10 w-10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5"/></svg>
                </div>
                <p class="settings-kicker">"THISCORD"</p>
                <h2 id="update-dialog-title" class="text-3xl font-bold tracking-tight">{move || crate::i18n::t("A new version of Thiscord is available")}</h2>
                <p class="text-lg text-white/80">{move || {let s=ui.updates.get();thiscord_frontend::localized_format!(crate::i18n::language(), "Version {} is ready. You have version {}.",s.available_version.unwrap_or_default(),s.current_version)}}</p>
                <p class="max-w-xl text-sm text-white/60">{move || crate::i18n::t("Thiscord downloads and verifies the update, then restarts to finish installing. Nothing is installed until you choose Download and install.")}</p>
                <Show when=downloading>
                    <div class="w-full max-w-md">
                        <progress class="w-full" max="100" value=move||{let s=ui.updates.get();s.total.filter(|n|*n>0).map(|n|(100.0*s.downloaded as f64/n as f64).min(100.0))}/>
                        <p class="text-sm text-white/60">{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Downloaded {} MiB",crate::i18n::number(ui.updates.get().downloaded as f64/(1024.0*1024.0),1))}</p>
                    </div>
                </Show>
                <p role="status" class="text-sm text-white/70">{move||crate::i18n::message(ui.updates.get().message)}</p>
                <div class="flex flex-wrap justify-center gap-3">
                    <button class="rounded-lg bg-brand px-6 py-3 text-base font-semibold text-white hover:brightness-110 disabled:opacity-50" disabled=move||busy(ui)||ui.voice.get().channel_id.is_some() on:click=move |_|request(ui,"update_install")>{move || crate::i18n::t("Download and install")}</button>
                    <button class="rounded-lg bg-white/10 px-6 py-3 text-base text-white/80 hover:bg-white/15 disabled:opacity-50" disabled=installing on:click=move |_|later()>{move || crate::i18n::t("Not now")}</button>
                </div>
                <Show when=move||ui.voice.get().channel_id.is_some()><p class="text-sm text-white/60">{move || crate::i18n::t("Disconnect from voice before installing.")}</p></Show>
                <p role="alert" class="text-sm text-red-300">{move||crate::i18n::message(ui.update_error.get())}</p>
                <p class="text-xs text-white/40">{move || crate::i18n::t("You can also install later from Settings → Updates.")}</p>
            </div>
        </dialog>
    }
}

#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {<div class="settings-card">
        <h2 class="settings-panel-title text-xl font-semibold">{move || crate::i18n::t("Application updates")}</h2>
        <Show when=desktop fallback=||view!{<p>{move || crate::i18n::t("Refresh the browser to load the latest web version.")}</p>}>
            <p>{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Installed version: {}",ui.updates.get().current_version)}</p>
            <p class="text-sm text-white/60">{move || crate::i18n::t("Thiscord checks for updates when it starts and when a new release is announced. You can also check manually. Updates install only when you choose Install and restart.")}</p>
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
