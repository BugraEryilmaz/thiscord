use leptos::prelude::*;

/// Decorative mark; the surrounding link or button supplies its accessible name.
#[component]
pub(crate) fn BrandMark(class: &'static str) -> impl IntoView {
    view! { <span class=format!("brand-mark {class}") aria-hidden="true"></span> }
}
