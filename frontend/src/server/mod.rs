pub mod channels;
use leptos::prelude::*;

use shared::models::Server;

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "server.css"
);

#[component]
pub fn ServerComponent(active_server: RwSignal<Option<Server>>) -> impl IntoView {
    let _ = active_server; // Use this signal to manage the active server state
    view! {
        <div class=style::server_container>
            <p>"This is a placeholder for the server component."</p>
        </div>
    }
}