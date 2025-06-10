use leptos::prelude::*;

stylance::import_style!(#[allow(dead_code)] style, "home.css");


#[component]
pub fn Sidebar () -> impl IntoView {
    
    view! {
        <div class=style::sidebar>
            <ol>
            </ol>
        </div>
    }
}