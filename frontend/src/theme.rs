//! Reactive appearance state for each WebView. Only the theme identifier and
//! accent color are stored, on this device; storage events keep the overlay,
//! notification window and browser tabs in step.
use leptos::prelude::*;
use thiscord_frontend::appearance::Appearance;
use wasm_bindgen::JsCast;

const THEME_KEY: &str = "thiscord.theme";
const ACCENT_KEY: &str = "thiscord.accent";
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
    let theme = storage.set_item(THEME_KEY, appearance.theme.id()).is_ok();
    let accent = match appearance.accent {
        Some(accent) => storage.set_item(ACCENT_KEY, &accent.hex()).is_ok(),
        None => storage.remove_item(ACCENT_KEY).is_ok(),
    };
    theme && accent
}

fn saved() -> Appearance {
    let storage = window().local_storage().ok().flatten();
    let item = |key| {
        storage
            .as_ref()
            .and_then(|storage| storage.get_item(key).ok().flatten())
    };
    Appearance::resolve(item(THEME_KEY).as_deref(), item(ACCENT_KEY).as_deref())
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
        let _ = style.set_property(name, &value);
    }
}

pub fn initialize() {
    let initial = saved();
    // Apply before the first render so a custom theme never flashes the default.
    apply(initial);
    APPEARANCE.with(|appearance| appearance.set(initial));
    Effect::new(move |_| apply(current()));
    let listener = window_event_listener(leptos::ev::storage, move |event| {
        if event
            .key()
            .as_deref()
            .is_none_or(|key| key == THEME_KEY || key == ACCENT_KEY)
        {
            APPEARANCE.with(|appearance| appearance.set(saved()));
        }
    });
    on_cleanup(move || listener.remove());
}
