mod channels;

use leptos::prelude::*;

use channels::Channels;
use front_shared::Server;

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "server.css"
);

#[component]
pub fn ServerComponent(active_server: ReadSignal<Option<Server>>) -> impl IntoView {
    view! {
        <div class=style::server_container>
            <Channels active_server=active_server />
            <p>"This is a placeholder for the server component."</p>
        </div>
    }
}