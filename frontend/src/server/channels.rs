use std::vec;

use leptos::logging::log;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::models::ChannelWithUsers;
use shared::models::JoinChannel;
use shared::models::Server;
use uuid::Uuid;

use crate::utils::invoke;

#[derive(serde::Serialize, serde::Deserialize)]
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
    "channels.css"
);

#[component]
pub fn Channels(active_server: RwSignal<Option<Server>>) -> impl IntoView {
    let channels = LocalResource::new(move || async move {
        if let Some(server) = active_server.get() {
            get_channels(server.id).await.map(|channels| {
                channels
                    .into_iter()
                    .partition(|channel| channel.channel.type_ == shared::models::ChannelType::Text)
            })
        } else {
            Ok((vec![], vec![]))
        }
    });
    view! {
        <div class=style::channel_list_container>
            <Suspense fallback=move || view! { <p>"Loading..."</p> }>
                {move || Suspend::new(async move {
                    match channels.await {
                        Ok((text_channels, voice_channels)) => {
                            view! {
                                <ChannelList
                                    server_name=active_server
                                    text_channels=text_channels
                                    voice_channels=voice_channels
                                />
                            }.into_any()
                        },
                        Err(e) => {
                            view! { <p>{format!("Failed to load channels: {}", e)}</p> }.into_any()
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
    server_name: RwSignal<Option<Server>>,
    text_channels: Vec<ChannelWithUsers>,
    voice_channels: Vec<ChannelWithUsers>,
) -> impl IntoView {
    let (show_text_channels, set_show_text_channels) = signal(true);
    let (show_voice_channels, set_show_voice_channels) = signal(true);
    let (text_channels, _set_text_channels) = signal(text_channels);
    let (voice_channels, _set_voice_channels) = signal(voice_channels);
    view! {
        <ul class=style::channel_list>
            <li class=style::channel_list_servername>
                <h2>{ move || server_name.get().map_or("No active server".to_string(), |s| s.name.clone()) }</h2>
            </li>
            <li class=style::channel_list_groupname
                on:click=move |_| {
                    set_show_text_channels.update(|v| *v = !*v);
                }
            >
                <h2>"Text Channels" </h2>
                <div class={move || if show_text_channels.get() { style::channel_list_down } else { style::channel_list_right }}>
                    <h2>"❭"</h2>
                </div>
            </li>
            <Show
                when=move || show_text_channels.get()
                fallback=move || view! { }
            >
                <For
                    each=move || text_channels.get()
                    key=|channel| channel.channel.id
                    children=move |channel| view! {
                        <li class=style::channel_list_item>
                            <h3>{ channel.channel.name }</h3>
                        </li>
                    }
                />
            </Show>
            <li class=style::channel_list_groupname
                on:click=move |_| {
                    set_show_voice_channels.update(|v| *v = !*v);
                }
            >
                <h2>"Voice Channels" </h2>
                <div class={move || if show_voice_channels.get() { style::channel_list_down } else { style::channel_list_right }}>
                    <h2>"❭"</h2>
                </div>
            </li>
            <Show
                when=move || show_voice_channels.get()
                fallback=move || view! { }
            >
                <For
                    each=move || voice_channels.get()
                    key=|channel| channel.channel.id
                    children=move |channel| view! {
                        <ChannelItem channel=channel.clone()
                            on:click=move |_| {
                                let channel_name = channel.channel.name.clone();
                                spawn_local(async move {
                                    let join_args = JoinChannel {
                                        server_id: channel.channel.server_id,
                                        channel_id: channel.channel.id,
                                    };
                                    let join_args = serde_wasm_bindgen::to_value(&join_args).unwrap();
                                    let result = invoke("join_channel", join_args).await;
                                    if let Err(e) = result {
                                        log!("Failed to join channel: {:?}", e);
                                    } else {
                                        log!("Joined channel: {:?}", channel_name);
                                    }
                                });
                            }
                        />
                    }
                />
            </Show>
        </ul>
    }
}

#[component]
pub fn ChannelItem(channel: ChannelWithUsers) -> impl IntoView {
    view! {
        <li class=style::channel_list_item>
            <h3>{ channel.channel.name }</h3>
            <span>
                { channel.users.len() } " users"
            </span>
            </li>
        <ul>
            <For
                each=move || channel.users.clone()
                key=|user| user.id
                children=move |user| view! {
                    <li>
                        <span class=style::channel_user>
                            { user.username.clone() }
                        </span>
                    </li>
                }
            />
        </ul>
    }
}
