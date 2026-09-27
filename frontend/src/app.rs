use gloo_net::http::Request;
use leptos::prelude::*;
use thiscord_shared::{HEALTH_PATH, HealthResponse, HealthStatus};

#[component]
pub fn App() -> impl IntoView {
    let (status, set_status) = signal("Backend not checked".to_owned());
    let (checking, set_checking) = signal(false);
    let check_backend = move |_| {
        set_checking.set(true);
        set_status.set("Connecting…".into());
        leptos::task::spawn_local(async move {
            let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
            let result = async {
                let response =
                    Request::get(&format!("{}{HEALTH_PATH}", base.trim_end_matches('/')))
                        .send()
                        .await
                        .map_err(|error| error.to_string())?;
                if !response.ok() {
                    return Err(format!("Backend returned HTTP {}", response.status()));
                }
                response
                    .json::<HealthResponse>()
                    .await
                    .map_err(|error| error.to_string())
            }
            .await;
            set_status.set(match result {
                Ok(HealthResponse {
                    status: HealthStatus::Ok,
                }) => "Backend is online".into(),
                Err(error) => format!("Could not reach backend: {error}"),
            });
            set_checking.set(false);
        });
    };

    view! {
        <main class="mx-auto my-[12vh] max-w-2xl space-y-4 p-8">
            <h1 class="text-[2.5rem] font-bold">"Thiscord"</h1>
            <p class="leading-relaxed">"Your place to chat and hang out."</p>
            <p class="leading-relaxed">"The project is ready for its first features."</p>
            <button
                class="cursor-pointer rounded-md bg-brand px-4 py-3 text-white focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-white disabled:cursor-wait disabled:opacity-60"
                on:click=check_backend
                disabled=move || checking.get()
            >
                "Check backend"
            </button>
            <p class="break-words leading-relaxed" role="status">{move || status.get()}</p>
        </main>
    }
}
