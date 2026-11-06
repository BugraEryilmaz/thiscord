//! Detected game activity ("Playing …"): desktop detection state, the device
//! setting and display labels. The friends socket shares it (see `friends`).
use super::Ui;
use crate::account_client::{desktop, native};
use leptos::prelude::*;
use serde_json::json;
use thiscord_frontend::game_activity::{ACTIVITY_EVENT, ActivityStatus};
use thiscord_shared::account::{Activity, ActivityKind};

/// Plain-text label ("Playing Factorio"); never markup.
pub(super) fn label(activity: &Activity) -> String {
    match activity.kind {
        ActivityKind::Playing => thiscord_frontend::localized_format!(
            crate::i18n::language(),
            "Playing {}",
            activity.name
        ),
    }
}

/// This device's shared activity, for the account ribbon.
pub(super) fn own(ui: Ui) -> Option<String> {
    ui.activity.with(|s| s.activity.as_ref().map(label))
}

/// Follows native detection for the lifetime of the app (desktop only).
#[component]
pub(super) fn Host(ui: Ui) -> impl IntoView {
    let listener = StoredValue::new_local(None::<crate::account_client::EventListener>);
    on_cleanup(move || listener.set_value(None));
    if desktop() {
        leptos::task::spawn_local(async move {
            let Ok(handle) =
                crate::account_client::listen(ACTIVITY_EVENT, move |status: ActivityStatus| {
                    if !ui.activity.is_disposed() {
                        ui.activity.set(status);
                    }
                })
                .await
            else {
                return;
            };
            if listener.is_disposed() {
                return;
            }
            listener.set_value(Some(handle));
            if let Ok(status) = native::<ActivityStatus>("activity_status", json!({})).await
                && !ui.activity.is_disposed()
            {
                ui.activity.set(status);
            }
        });
    }
}

#[component]
pub(super) fn Settings(ui: Ui) -> impl IntoView {
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    view! {
        <div class="settings-card">
            <h2 class="settings-panel-title text-xl font-semibold">{move || crate::i18n::t("Activity status")}</h2>
            <label class="flex items-center gap-3">
                <input type="checkbox" prop:checked=move || ui.activity.with(|s| s.enabled)
                    disabled=move || busy.get()
                    on:change=move |ev| {
                        if busy.get_untracked() { return; }
                        let previous = ui.activity.get_untracked();
                        let value = event_target_checked(&ev);
                        ui.activity.update(|s| { s.enabled = value; if !value { s.activity = None; } });
                        busy.set(true);
                        error.set(String::new());
                        leptos::task::spawn_local(async move {
                            let result = native::<ActivityStatus>("activity_set_enabled", json!({"enabled":value})).await;
                            if busy.try_get_untracked().is_none() { return; }
                            match result {
                                Ok(status) => ui.activity.set(status),
                                Err(message) => { ui.activity.set(previous); error.set(message); }
                            }
                            busy.set(false);
                        });
                    }/>
                {move || crate::i18n::t("Show the game I'm playing as my status")}
            </label>
            <p class="settings-hint">{move || crate::i18n::t("Thiscord looks for known games among the programs running on this computer. Only the game's name is shared, with friends and members of your servers, while Thiscord is open.")}</p>
            <p class="settings-hint">{move || crate::i18n::t("On by default. Changes save automatically on this computer.")}</p>
            <Show when=move || ui.activity.with(|s| s.enabled)>
                <p role="status">{move || ui.activity.with(|s| s.activity.as_ref().map_or_else(
                    || crate::i18n::t("No game detected right now.").to_owned(),
                    |a| thiscord_frontend::localized_format!(crate::i18n::language(), "Detected: {}", label(a)),
                ))}</p>
            </Show>
            <p role="status" class="text-red-300">{move || crate::i18n::message(error.get())}</p>
        </div>
    }
}
