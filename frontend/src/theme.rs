//! Reactive appearance state for each WebView. The theme code (theme and
//! custom colors) is stored on this device; storage events keep the overlay,
//! notification window and browser tabs in step.
use leptos::prelude::*;
use thiscord_frontend::appearance::Appearance;
use wasm_bindgen::JsCast;

const STORAGE_KEY: &str = "thiscord.appearance";
/// Client 0.1.53 stored the theme and accent separately.
const LEGACY_KEYS: [&str; 2] = ["thiscord.theme", "thiscord.accent"];
thread_local! {
    static APPEARANCE: ArcRwSignal<Appearance> = ArcRwSignal::new(Appearance::default());
}

pub fn current() -> Appearance {
    APPEARANCE.with(|appearance| appearance.get())
}

pub fn current_untracked() -> Appearance {
    APPEARANCE.with(|appearance| appearance.get_untracked())
}

/// Apply without saving, for live previews while a color picker is open.
pub fn preview(appearance: Appearance) {
    APPEARANCE.with(|current| current.set(appearance));
}

/// Apply and remember on this device. Returns false when storage is
/// unavailable; the change still applies until this window closes.
pub fn save(appearance: Appearance) -> bool {
    preview(appearance);
    let Some(storage) = window().local_storage().ok().flatten() else {
        return false;
    };
    let saved = if appearance == Appearance::default() {
        storage.remove_item(STORAGE_KEY).is_ok()
    } else {
        storage.set_item(STORAGE_KEY, &appearance.encode()).is_ok()
    };
    if saved {
        for key in LEGACY_KEYS {
            let _ = storage.remove_item(key);
        }
    }
    saved
}

fn saved() -> Appearance {
    let storage = window().local_storage().ok().flatten();
    let item = |key| {
        storage
            .as_ref()
            .and_then(|storage| storage.get_item(key).ok().flatten())
    };
    match item(STORAGE_KEY) {
        Some(code) => Appearance::resolve(Some(&code)),
        None => Appearance::legacy(
            item(LEGACY_KEYS[0]).as_deref(),
            item(LEGACY_KEYS[1]).as_deref(),
        ),
    }
}

/// Values are validated `#rrggbb` colors, set through CSSOM so the
/// `style-src 'self'` CSP keeps blocking inline style attributes.
fn apply(appearance: Appearance) {
    let Some(root) = document()
        .document_element()
        .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let style = root.style();
    for (name, value) in appearance.properties() {
        let _ = match value {
            Some(value) => style.set_property(name, &value),
            None => style.remove_property(name).map(drop),
        };
    }
}

pub fn initialize() {
    let initial = saved();
    // Apply before the first render so a custom theme never flashes the default.
    apply(initial);
    APPEARANCE.with(|appearance| appearance.set(initial));
    Effect::new(move |_| apply(current()));
    let listener = window_event_listener(leptos::ev::storage, move |event| {
        if event.key().as_deref().is_none_or(|key| key == STORAGE_KEY) {
            APPEARANCE.with(|appearance| appearance.set(saved()));
        }
    });
    on_cleanup(move || listener.remove());
}
