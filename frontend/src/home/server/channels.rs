use std::vec;

use leptos::prelude::*;
use front_shared::{ChannelWithUsers, Server};
use uuid::Uuid;

use crate::utils::invoke;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct GetChannels {
    server_id: Uuid,
}

async fn get_channels(server_id: Uuid) -> Result<Vec<ChannelWithUsers>, String> {
    let arg = GetChannels { server_id };
    let arg = serde_wasm_bindgen::to_value(&arg).unwrap();
    let channels = invoke("get_channels", arg)
        .await
        .map_err(|e| e.as_string().unwrap_or_default())?;
    let channels: Vec<ChannelWithUsers> =
        serde_wasm_bindgen::from_value(channels).map_err(|e| e.to_string())?;
    Ok(channels)
}

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "server.css"
);

#[component]
pub fn Channels(active_server: ReadSignal<Option<Server>>) -> impl IntoView {
    let channels = LocalResource::new(move || async move {
        if let Some(server) = active_server.get() {
            get_channels(server.id).await
        } else {
            Ok(vec![])
        }
    });
    view! {
        <div class=style::channel_list_container>
            <Suspense fallback=move || view! { <p>"Loading..."</p> }>
                {move || Suspend::new(async move {
                    match channels.await {
                        Ok(channels) => {
                            view! {
                                <ChannelList channels=move || channels.clone() />
                            }.into_any()
                        },
                        Err(_e) => {
                            view! { <p>"Failed to load channels."</p> }.into_any()
                        },
                    }
                })}
            </Suspense>
        </div>
    }
}

#[component]
pub fn Loading() -> impl IntoView {
    view! {
        <div class="loading">
            <p>"Loading channels..."</p>
        </div>
    }
}

#[component]
pub fn ChannelList(
    channels: impl Fn() -> Vec<ChannelWithUsers> + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <ul>
            <For
                each=move || channels()
                key=|channel| channel.channel.id
                children=move |channel| view! {
                    <li>
                        <h3>{ channel.channel.name }</h3>
                    </li>
                }
            />
        </ul>
    }
}
