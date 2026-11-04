//! Account-level custom status: ribbon button and set/clear dialog.
use super::{
    Ui, client,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_frontend::member_list::{StatusExpiry, status_text};
use thiscord_shared::account::{
    AccountRequest, AccountResponse, CustomStatus, CustomStatusUpdate, MAX_STATUS_CHARS,
};

/// The signed-in account's unexpired status line, if any.
pub(super) fn own(ui: Ui) -> Option<String> {
    ui.account.with(|a| {
        a.as_ref()
            .and_then(|a| a.custom_status.as_ref())
            .map(status_text)
    })
}

/// Applies a server-confirmed status (from this or another device).
pub(super) fn apply(ui: Ui, status: Option<CustomStatus>) {
    if ui
        .account
        .with_untracked(|a| a.as_ref().is_some_and(|a| a.custom_status != status))
    {
        ui.account.update(|a| {
            if let Some(a) = a {
                a.custom_status = status;
            }
        });
    }
}

/// Seconds until the device's next local midnight.
fn until_midnight() -> f64 {
    let now = js_sys::Date::new_0();
    let midnight = js_sys::Date::new_with_year_month_day(
        now.get_full_year(),
        now.get_month() as i32,
        now.get_date() as i32 + 1,
    );
    (midnight.get_time() - now.get_time()) / 1000.0
}

/// Ribbon button opening the custom status dialog.
#[component]
pub(super) fn StatusButton(ui: Ui) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let emoji = RwSignal::new(None::<String>);
    let text = RwSignal::new(String::new());
    let expiry = RwSignal::new(StatusExpiry::Never);
    let picking = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let open = move |_| {
        let current = ui
            .account
            .with_untracked(|a| a.as_ref().and_then(|a| a.custom_status.clone()));
        emoji.set(current.as_ref().and_then(|s| s.emoji.clone()));
        text.set(current.map(|s| s.text).unwrap_or_default());
        expiry.set(StatusExpiry::Never);
        picking.set(false);
        error.set(String::new());
        if let Some(dialog) = dialog.get() {
            super::dialog::open(&dialog);
        }
    };
    let save = move |status: Option<CustomStatusUpdate>| {
        if let Some(status) = &status
            && let Err(message) = status.validate()
        {
            error.set(message.into());
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        saving.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result =
                client::request(&AccountRequest::SetCustomStatus { status }, Some(&token)).await;
            // Never apply an old session's result to a newly signed-in account.
            if ui.token.get_untracked().as_deref() != Some(&token) {
                return;
            }
            saving.set(false);
            match result {
                Ok(AccountResponse::Account { account }) => {
                    apply(ui, account.custom_status);
                    if let Some(dialog) = dialog.get_untracked() {
                        dialog.close();
                    }
                }
                Ok(_) => error.set("Unexpected server response".into()),
                Err(e) => error.set(e),
            }
        });
    };
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        save(Some(CustomStatusUpdate {
            emoji: emoji.get_untracked(),
            text: text.get_untracked(),
            expires_in: expiry.get_untracked().seconds(until_midnight()),
        }));
    };
    let pick = Callback::new(move |chosen: &'static str| {
        emoji.set(Some(chosen.to_owned()));
        picking.set(false);
    });
    view! {
        <button type="button" class="dock-status icon-button shrink-0" aria-haspopup="dialog"
            aria-label=move || crate::i18n::t("Set custom status")
            data-tip=move || own(ui).unwrap_or_else(|| crate::i18n::t("Set custom status").to_owned())
            on:click=open><Icon kind=Glyph::Status/></button>
        <dialog node_ref=dialog class="context-dialog" aria-labelledby="custom-status-title"
            on:click=move |e| super::dialog::dismiss_outside(e, dialog, saving.get_untracked())
            on:cancel=move |ev: web_sys::Event| if saving.get_untracked() { ev.prevent_default(); }>
            <header class="mb-4 flex items-center justify-between gap-3">
                <h2 id="custom-status-title" class="text-xl font-semibold">{move || crate::i18n::t("Set custom status")}</h2>
                <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Close dialog") disabled=move || saving.get()
                    on:click=move |_| if let Some(d) = dialog.get() { d.close(); }><Icon kind=Glyph::Close/></button>
            </header>
            <form class="space-y-4" on:submit=submit>
                <div class="flex items-end gap-2">
                    <div class="relative">
                        <button type="button" class="grid size-11 place-items-center rounded-lg bg-black/20 text-xl"
                            aria-label=move || emoji.get().map_or_else(|| crate::i18n::t("Choose an emoji").to_owned(), |e| thiscord_frontend::localized_format!(crate::i18n::language(), "Emoji: {}", e))
                            aria-haspopup="dialog" aria-expanded=move || picking.get().to_string()
                            on:click=move |_| picking.update(|p| *p = !*p)>
                            {move || emoji.get().map_or_else(|| view! {<Icon kind=Glyph::Emoji/>}.into_any(), |e| e.into_any())}
                        </button>
                        <Show when=move || picking.get()>
                            <div class="emoji-popover status-emoji-popover" role="dialog" aria-label=move || crate::i18n::t("Emoji")
                                on:keydown=move |e| if e.key() == "Escape" { e.prevent_default(); e.stop_propagation(); picking.set(false); }>
                                <super::emoji::EmojiGrid pick/>
                            </div>
                        </Show>
                    </div>
                    <label class="settings-field min-w-0 flex-1">
                        <span>{move || crate::i18n::t("What's happening?")}</span>
                        <input class="w-full rounded-md border border-white/20 bg-black/20 px-3 py-2 focus:border-brand focus:outline-none"
                            maxlength=MAX_STATUS_CHARS.to_string() autocomplete="off"
                            prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))/>
                        <small>{move || format!("{}/{MAX_STATUS_CHARS}", text.with(|t| t.chars().count()))}</small>
                    </label>
                </div>
                <Show when=move || emoji.get().is_some()>
                    <button type="button" class="text-sm text-brand underline" on:click=move |_| emoji.set(None)>{move || crate::i18n::t("Remove emoji")}</button>
                </Show>
                <label class="settings-field">
                    <span>{move || crate::i18n::t("Clear after")}</span>
                    <select class="w-full rounded bg-slate-900 p-2" prop:value=move || expiry.get().key()
                        on:change=move |e| expiry.set(StatusExpiry::from_key(&event_target_value(&e)))>
                        {StatusExpiry::ALL.map(|preset| view! {<option value=preset.key()>{move || crate::i18n::t(preset.label())}</option>}).collect_view()}
                    </select>
                </label>
                <p class="text-xs text-white/60">{move || crate::i18n::t("Friends and members of your servers can see your status.")}</p>
                <p class="text-sm text-red-300" role="alert" hidden=move || error.with(String::is_empty)>{move || crate::i18n::message(error.get())}</p>
                <div class="flex flex-wrap justify-end gap-2">
                    <Show when=move || ui.account.with(|a| a.as_ref().is_some_and(|a| a.custom_status.is_some()))>
                        <button type="button" class="rounded bg-white/10 px-4 py-2 text-sm" disabled=move || saving.get()
                            on:click=move |_| save(None)>{move || crate::i18n::t("Clear status")}</button>
                    </Show>
                    <button class="rounded bg-brand px-4 py-2 text-sm font-semibold disabled:opacity-50"
                        disabled=move || saving.get() || (emoji.with(Option::is_none) && text.with(|t| t.trim().is_empty()))>
                        {move || crate::i18n::t("Save status")}</button>
                </div>
            </form>
        </dialog>
    }
}
