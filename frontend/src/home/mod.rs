mod leftpanel;
mod login;

use leptos::{context, prelude::*};

use crate::app::SessionCookieSignal;

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "home.css"
);

#[component]
pub fn Home() -> impl IntoView {
    let cookie =
        context::use_context::<SessionCookieSignal>().expect("SessionCookie context not found");

    view! {
        <main class=style::home_container>
            <leftpanel::Sidebar />
            <Show when=move || cookie.get() fallback=move || view! { <login::Login /> }>
                <h1>"Welcome to the Home Page"</h1>
            </Show>
        </main>
    }
}
