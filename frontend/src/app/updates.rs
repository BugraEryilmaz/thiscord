use super::Ui;
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_frontend::changelog::{Changelog, Kind, Span};
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
            <button class="fixed bottom-4 right-4 z-30 rounded-lg bg-surface px-3 py-2 text-sm text-white/70 hover:text-white" on:click=move |_|{if let Some(d)=dialog.get(){super::dialog::open(&d);}}>{move || crate::i18n::t("Updates")}</button>
        </Show>
        <dialog node_ref=dialog aria-label=move || crate::i18n::t("Application updates") class="m-auto max-h-[85vh] w-[calc(100%-2rem)] max-w-xl overflow-y-auto rounded-xl border border-white/20 bg-surface p-6 text-white backdrop:bg-black/70">
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
            super::dialog::open(&d);
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
    let log = changelog(ui);
    view! {
        <dialog node_ref=dialog class="update-dialog" aria-labelledby="update-dialog-title" on:cancel=move |e: web_sys::Event|{e.prevent_default();later();}>
            <div class="flex h-full flex-col">
                <div class="flex min-h-0 flex-1 flex-col overflow-y-auto px-6 focus:outline-none sm:px-10">
                    <header class="flex flex-col items-center gap-2 pt-7 pb-6 text-center" class:my-auto=move||log.with(Option::is_none)>
                        <div class="update-hero-icon" aria-hidden="true">
                            <svg class="h-7 w-7" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5"/></svg>
                        </div>
                        <p class="settings-kicker">"THISCORD"</p>
                        <h2 id="update-dialog-title" class="font-display text-3xl font-bold tracking-tight">{move || crate::i18n::t("A new version of Thiscord is available")}</h2>
                        <p class="sr-only">{move || {let s=ui.updates.get();thiscord_frontend::localized_format!(crate::i18n::language(), "Version {} is ready. You have version {}.",s.available_version.unwrap_or_default(),s.current_version)}}</p>
                        <VersionStep ui=ui/>
                    </header>
                    <Show when=move||log.with(Option::is_some)>
                        <div class="mx-auto max-w-2xl pb-8"><ChangelogView log=log/></div>
                    </Show>
                </div>
                <footer class="update-footer">
                    <Show when=downloading>
                        <div class="w-full max-w-md">
                            <progress class="w-full" max="100" value=move||{let s=ui.updates.get();s.total.filter(|n|*n>0).map(|n|(100.0*s.downloaded as f64/n as f64).min(100.0))}/>
                            <p class="text-sm text-white/60">{move||thiscord_frontend::localized_format!(crate::i18n::language(), "Downloaded {} MiB",crate::i18n::number(ui.updates.get().downloaded as f64/(1024.0*1024.0),1))}</p>
                        </div>
                    </Show>
                    <p role="status" class="text-sm text-white/70 empty:hidden">{move||crate::i18n::message(ui.updates.get().message)}</p>
                    <div class="flex flex-wrap justify-center gap-3">
                        <button class="rounded-lg bg-brand px-6 py-3 text-base font-semibold text-white hover:brightness-110 disabled:opacity-50" disabled=move||busy(ui)||ui.voice.get().channel_id.is_some() on:click=move |_|request(ui,"update_install")>{move || crate::i18n::t("Download and install")}</button>
                        <button class="rounded-lg bg-white/10 px-6 py-3 text-base text-white/80 hover:bg-white/15 disabled:opacity-50" disabled=installing on:click=move |_|later()>{move || crate::i18n::t("Not now")}</button>
                    </div>
                    <Show when=move||ui.voice.get().channel_id.is_some()><p class="text-sm text-white/60">{move || crate::i18n::t("Disconnect from voice before installing.")}</p></Show>
                    <p role="alert" class="text-sm text-red-300 empty:hidden">{move||crate::i18n::message(ui.update_error.get())}</p>
                    <p class="max-w-xl text-xs text-white/60">{move || crate::i18n::t("Thiscord downloads and verifies the update, then restarts to finish installing. Nothing is installed until you choose Download and install.")}" "{move || crate::i18n::t("You can also install later from Settings → Updates.")}</p>
                </footer>
            </div>
        </dialog>
    }
}

/// "0.1.58 → 0.1.59", with the installed version dimmed.
#[component]
fn VersionStep(ui: Ui) -> impl IntoView {
    view! {
        <p class="update-version-step" aria-hidden="true">
            <span class="update-version">{move||ui.updates.get().current_version}</span>
            <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14m-6-6 6 6-6 6"/></svg>
            <span class="update-version update-version-new">{move||ui.updates.get().available_version.unwrap_or_default()}</span>
        </p>
    }
}

/// The available release's changelog. Download progress updates the status many
/// times per second, so the notes are compared first and parsed only on change.
fn changelog(ui: Ui) -> Memo<Option<Changelog>> {
    let notes = Memo::new(move |_| ui.updates.with(|s| s.notes.clone()));
    Memo::new(move |_| {
        notes
            .with(|n| n.as_deref().map(Changelog::parse))
            .filter(|log| !log.is_empty())
    })
}

/// Highlights up front; every grouped change behind "Show all changes", or shown
/// directly when the notes have no highlights.
#[component]
fn ChangelogView(log: Memo<Option<Changelog>>) -> impl IntoView {
    use std::sync::atomic::{AtomicU32, Ordering};
    // The prompt and the settings panel can both be mounted.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!("changelog-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let expanded = RwSignal::new(false);
    move || {
        let log = log.get()?;
        let count = log.detail_count();
        let collapsible = !log.highlights.is_empty();
        let (title_id, details_id) = (format!("{id}-title"), format!("{id}-details"));
        let controls = details_id.clone();
        Some(view! {
            <section class="changelog" aria-labelledby=title_id.clone()>
                <h3 id=title_id.clone() class="changelog-title">{move || crate::i18n::t("What's new")}</h3>
                <ul class="changelog-highlights">
                    {log.highlights.into_iter().map(|entry| view! {
                        <li class="changelog-highlight">
                            <span class="changelog-highlight-icon" aria-hidden="true">
                                <svg class="h-4 w-4" viewBox="0 0 24 24" fill="currentColor"><path d="M12 2.5l2.1 6.2a2 2 0 0 0 1.2 1.2l6.2 2.1-6.2 2.1a2 2 0 0 0-1.2 1.2L12 21.5l-2.1-6.2a2 2 0 0 0-1.2-1.2L2.5 12l6.2-2.1a2 2 0 0 0 1.2-1.2z"/></svg>
                            </span>
                            <div class="min-w-0">
                                {entry.title.map(|title| view!{<p class="changelog-highlight-title">{title}</p>})}
                                <p class="changelog-highlight-text">{spans(entry.text)}</p>
                            </div>
                        </li>
                    }).collect_view()}
                </ul>
                {(count > 0).then(|| view! {
                    {collapsible.then(|| view! {
                        <button type="button" class="changelog-toggle" aria-expanded=move||expanded.get().to_string() aria-controls=controls on:click=move |_|expanded.update(|e| *e = !*e)>
                            <span>{move || if expanded.get() {crate::i18n::t("Show fewer changes").to_owned()} else {thiscord_frontend::localized_format!(crate::i18n::language(), "Show all changes ({})", count)}}</span>
                            <svg class="changelog-chevron h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg>
                        </button>
                    })}
                    <div id=details_id class="changelog-details" hidden=move||collapsible && !expanded.get()>
                        {log.sections.into_iter().map(|section| {
                            let (class, label) = match section.kind {
                                Kind::New => ("changelog-badge changelog-badge-new", crate::i18n::t("New").to_owned()),
                                Kind::Improved => ("changelog-badge changelog-badge-improved", crate::i18n::t("Improved").to_owned()),
                                Kind::Fixed => ("changelog-badge changelog-badge-fixed", crate::i18n::t("Fixed").to_owned()),
                                Kind::Other if section.title.is_empty() => ("changelog-badge", crate::i18n::t("Other changes").to_owned()),
                                Kind::Other => ("changelog-badge", section.title),
                            };
                            view! {
                                <div class="changelog-group">
                                    <h4><span class=class>{label}</span></h4>
                                    <ul class="changelog-list">
                                        {section.entries.into_iter().map(|entry| view! {
                                            <li>{entry.title.map(|title| view!{<strong class="changelog-entry-title">{title}". "</strong>})}{spans(entry.text)}</li>
                                        }).collect_view()}
                                    </ul>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                })}
            </section>
        })
    }
}

/// Changelog spans become text nodes only; release notes never reach innerHTML.
fn spans(spans: Vec<Span>) -> impl IntoView {
    spans
        .into_iter()
        .map(|span| match span {
            Span::Text(text) => text.into_any(),
            Span::Strong(text) => {
                view! {<strong class="font-semibold text-white">{text}</strong>}.into_any()
            }
            Span::Code(text) => view! {<code class="changelog-code">{text}</code>}.into_any(),
        })
        .collect_view()
}

#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    let log = changelog(ui);
    view! {<div class="settings-card">
        <h2 class="settings-panel-title text-xl font-semibold">{move || crate::i18n::t("Application updates")}</h2>
        <Show when=desktop fallback=||view!{
            <p>{move || crate::i18n::t("Refresh the browser to load the latest web version.")}</p>
            <p class="settings-hint">{move || crate::i18n::t("The desktop app adds global shortcuts, the in-game overlay and screen sharing with system audio, and installs updates when you choose.")}</p>
            <super::settings::DesktopAppLink class="settings-secondary"/>
        }>
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
            <Show when=move||ui.updates.with(|s|s.available_version.is_some())&&log.with(Option::is_some)>
                <div class="changelog-panel">
                    <VersionStep ui=ui/>
                    <ChangelogView log=log/>
                </div>
            </Show>
        </Show>
    </div>}
}
