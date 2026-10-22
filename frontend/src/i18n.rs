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

/// Display dates in the chosen locale, retaining the app's explicit UTC zone.
/// The wire timestamp remains unchanged.
pub fn datetime(timestamp: thiscord_shared::Timestamp) -> String {
    use js_sys::Intl::{DateTimeFormatOptions, DateTimeStyle};
    let options = DateTimeFormatOptions::new();
    options.set_date_style(DateTimeStyle::Medium);
    options.set_time_style(DateTimeStyle::Short);
    options.set_time_zone("UTC");
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
        timestamp.timestamp_millis() as f64,
    ));
    format!(
        "{} UTC",
        date.to_locale_string(language().code(), options.as_ref())
    )
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
pub fn LanguageSelector() -> impl IntoView {
    let failed = RwSignal::new(false);
    view! {
        <div class="space-y-2">
            <label class="flex flex-wrap items-center justify-between gap-3 text-sm">
                <span>{move || t("Language")}</span>
                <select class="rounded-md border border-white/20 bg-surface px-3 py-2 text-white"
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
