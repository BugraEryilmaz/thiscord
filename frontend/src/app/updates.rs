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
    let dismissed = RwSignal::new(None::<String>);
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    on_cleanup(move || abort.abort());
    if desktop() {
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    loop {
                        if let Ok(status) = native::<UpdateStatus>("update_status", json!({})).await
                        {
                            ui.updates.set(status);
                        }
                        gloo_timers::future::TimeoutFuture::new(1000).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
    view! {<Show when=desktop>
        <Show when=move||ui.account.get().is_none()>
            <button class="fixed bottom-4 right-4 z-30 rounded-lg bg-surface px-3 py-2 text-sm text-white/70 hover:text-white" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}>"Updates"</button>
        </Show>
        <Show when=move||ui.updates.get().available_version.is_some() && ui.updates.get().available_version!=dismissed.get()>
            <aside class="fixed bottom-20 right-4 z-40 w-[calc(100%-2rem)] max-w-sm space-y-3 rounded-xl border border-white/20 bg-surface p-4 text-white shadow-2xl" aria-label="App update">
                <div class="flex justify-between gap-4"><p class="font-semibold">{move||format!("Thiscord {} is available",ui.updates.get().available_version.unwrap_or_default())}</p>
                    <button aria-label="Dismiss update notification" class="text-sm text-white/60" on:click=move |_|dismissed.set(ui.updates.get_untracked().available_version)>"Later"</button>
                </div>
                <p class="text-sm text-white/70">"You choose when to install. Thiscord will restart."</p>
                <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||busy(ui)||ui.voice.get().channel_id.is_some() on:click=move |_|request(ui,"update_install")>"Install and restart"</button>
                <Show when=move||ui.voice.get().channel_id.is_some()><p class="text-sm text-white/60">"Disconnect from voice before installing."</p></Show>
                <Show when=move||busy(ui)><p class="text-sm" role="status">{move||ui.updates.get().message}</p></Show>
                <Show when=move||!ui.update_error.get().is_empty()><p role="alert" class="text-sm text-red-300">{move||ui.update_error.get()}</p></Show>
            </aside>
        </Show>
        <dialog node_ref=dialog aria-label="Application updates" class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-lg overflow-y-auto rounded-xl border border-white/20 bg-surface p-6 text-white backdrop:bg-black/70">
            <button class="float-right rounded px-2 py-1 hover:bg-white/10" aria-label="Close updates" on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>"Close"</button>
            <Panel ui=ui/>
        </dialog>
    </Show>}
}

#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    view! {<div class="space-y-4">
        <h2 class="text-xl font-semibold">"Application updates"</h2>
        <Show when=desktop fallback=||view!{<p>"Refresh the browser to load the latest web version."</p>}>
            <p>{move||format!("Installed version: {}",ui.updates.get().current_version)}</p>
            <p class="text-sm text-white/60">"Thiscord checks on launch and every six hours. Updates install only when you choose Install and restart."</p>
            <p role="status">{move||ui.updates.get().message}</p>
            <Show when=move||ui.updates.get().phase==UpdatePhase::Downloading>
                <progress class="w-full" max="100" value=move||{let s=ui.updates.get();s.total.filter(|n|*n>0).map(|n|(100.0*s.downloaded as f64/n as f64).min(100.0))}/>
                <p class="text-sm">{move||format!("Downloaded {:.1} MiB",ui.updates.get().downloaded as f64/(1024.0*1024.0))}</p>
            </Show>
            <div class="flex flex-wrap gap-3">
                <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-50" disabled=move||busy(ui)||ui.updates.get().phase==UpdatePhase::Unsupported on:click=move |_|request(ui,"update_check")>"Check for updates"</button>
                <Show when=move||ui.updates.get().available_version.is_some()>
                    <button class="rounded bg-brand px-4 py-2 disabled:opacity-50" disabled=move||busy(ui)||ui.voice.get().channel_id.is_some() on:click=move |_|request(ui,"update_install")>"Install and restart"</button>
                </Show>
            </div>
            <Show when=move||ui.voice.get().channel_id.is_some()><p class="text-sm text-white/60">"Disconnect from voice before installing."</p></Show>
            <p role="alert" class="text-sm text-red-300">{move||ui.update_error.get()}</p>
        </Show>
    </div>}
}
