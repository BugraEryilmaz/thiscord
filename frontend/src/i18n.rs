//! Reactive language state for each WebView. Only the language code is stored;
//! storage events synchronize the overlay, notification window and browser tabs.
use leptos::prelude::*;
use thiscord_frontend::localization::{self, Language};

const STORAGE_KEY: &str = "thiscord.language";
thread_local! {
    static LANGUAGE: ArcRwSignal<Language> = ArcRwSignal::new(Language::English);
}

pub fn language() -> Language {
    LANGUAGE.with(|language| language.get())
}

pub fn t(source: &str) -> &str {
    localization::translate(language(), source)
}

pub fn message(source: String) -> String {
    t(&source).to_owned()
}

pub fn snooze_label(duration: thiscord_shared::notifications::Snooze, source: &str) -> &str {
    if duration == thiscord_shared::notifications::Snooze::Resume {
        t("Resume notifications")
    } else {
        t(source)
    }
}

pub fn api_error(error: &thiscord_shared::ApiError) -> String {
    localization::api_error(LANGUAGE.with(|language| language.get_untracked()), error)
}

fn local_date(timestamp: thiscord_shared::Timestamp) -> js_sys::Date {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
        timestamp.timestamp_millis() as f64,
    ))
}

fn format_local(
    date: &js_sys::Date,
    date_style: Option<js_sys::Intl::DateTimeStyle>,
    time_style: Option<js_sys::Intl::DateTimeStyle>,
) -> String {
    let options = js_sys::Intl::DateTimeFormatOptions::new();
    if let Some(style) = date_style {
        options.set_date_style(style);
    }
    if let Some(style) = time_style {
        options.set_time_style(style);
    }
    date.to_locale_string(language().code(), options.as_ref())
        .into()
}

/// Display dates in the chosen locale and this device's time zone.
/// The wire timestamp remains UTC.
pub fn datetime(timestamp: thiscord_shared::Timestamp) -> String {
    use js_sys::Intl::DateTimeStyle;
    format_local(
        &local_date(timestamp),
        Some(DateTimeStyle::Medium),
        Some(DateTimeStyle::Short),
    )
}

/// Local hour and minute, used beside grouped follow-up messages.
pub fn clock(timestamp: thiscord_shared::Timestamp) -> String {
    format_local(
        &local_date(timestamp),
        None,
        Some(js_sys::Intl::DateTimeStyle::Short),
    )
}

/// Message header time: "Today at 14:05", "Yesterday at 14:05", or a full date.
pub fn message_time(timestamp: thiscord_shared::Timestamp) -> String {
    let date = local_date(timestamp);
    let day = |d: &js_sys::Date| (d.get_full_year(), d.get_month(), d.get_date());
    let today = js_sys::Date::new_0();
    let yesterday = js_sys::Date::new_0();
    yesterday.set_date(yesterday.get_date() - 1);
    let time = clock(timestamp);
    if day(&date) == day(&today) {
        thiscord_frontend::localized_format!(language(), "Today at {}", time)
    } else if day(&date) == day(&yesterday) {
        thiscord_frontend::localized_format!(language(), "Yesterday at {}", time)
    } else {
        datetime(timestamp)
    }
}

pub fn number(value: f64, decimals: u8) -> String {
    use js_sys::Intl::{NumberFormat, NumberFormatOptions};
    let options = NumberFormatOptions::new();
    options.set_minimum_fraction_digits(decimals);
    options.set_maximum_fraction_digits(decimals);
    let locales = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(language().code()));
    NumberFormat::new(&locales, options.as_ref())
        .format()
        .call1(
            &wasm_bindgen::JsValue::UNDEFINED,
            &wasm_bindgen::JsValue::from_f64(value),
        )
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| value.to_string())
}

fn saved_language() -> Language {
    let window = window();
    let saved = window
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten());
    let system: Vec<_> = window
        .navigator()
        .languages()
        .iter()
        .filter_map(|value| value.as_string())
        .collect();
    Language::resolve(saved.as_deref(), &system)
}

pub fn initialize() {
    LANGUAGE.with(|language| language.set(saved_language()));
    Effect::new(move |_| {
        if let Some(root) = document().document_element() {
            let _ = root.set_attribute("lang", language().code());
        }
    });
    let listener = window_event_listener(leptos::ev::storage, move |event| {
        if event.key().as_deref().is_none_or(|key| key == STORAGE_KEY) {
            LANGUAGE.with(|language| language.set(saved_language()));
        }
    });
    on_cleanup(move || listener.remove());
}

#[component]
pub fn LanguageSelector(
    /// A small select without a visible label, for corners of full-page screens.
    #[prop(optional)]
    compact: bool,
) -> impl IntoView {
    let failed = RwSignal::new(false);
    view! {
        <div class="space-y-2">
            <label class="flex flex-wrap items-center justify-between gap-3 text-sm">
                <span class:sr-only=compact>{move || t("Language")}</span>
                <select class="select-compact"
                    aria-label=move || t("Language")
                    prop:value=move || language().code()
                    on:change=move |event| {
                        if let Some(selected) = Language::from_tag(&event_target_value(&event)) {
                            LANGUAGE.with(|language| language.set(selected));
                            let saved = window().local_storage().ok().flatten()
                                .is_some_and(|storage| storage.set_item(STORAGE_KEY, selected.code()).is_ok());
                            failed.set(!saved);
                        }
                    }>
                    <option value="en" lang="en">"English"</option>
                    <option value="tr" lang="tr">"Türkçe"</option>
                </select>
            </label>
            <Show when=move || failed.get()>
                <p role="status" class="text-sm text-amber-200">{move || t("Your language preference could not be saved. It will apply until you close this window.")}</p>
            </Show>
        </div>
    }
}
