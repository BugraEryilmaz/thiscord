use std::time::Duration;
use std::vec;

use leptos::logging::log;
use leptos::logging::warn;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::models::AudioChannelMemberUpdate;
use shared::models::ChannelWithUsers;
use shared::models::JoinChannel;
use shared::models::Server;
use stylance::classes;
use uuid::Uuid;

use crate::utils::create_listener;
use crate::utils::hover_menu::HoverMenu;
use crate::utils::hover_menu::HoverMenuBackgroundStyle;
use crate::utils::hover_menu::HoverMenuDirection;
use crate::utils::hover_menu::HoverMenuTrigger;
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
    let channels_signal = RwSignal::new(None);
    // Fetch channels for the active server
    Effect::new(move || {
        channels_signal.set(None);
        if let Some(server) = active_server.get() {
            spawn_local(async move {
                match get_channels(server.id).await {
                    Ok(channels) => {
                        let channels: (Vec<_>, Vec<_>) =
                            channels.into_iter().partition(|channel| {
                                channel.channel.type_ == shared::models::ChannelType::Text
                            });
                        channels_signal.set(Some(Ok(channels)));
                    }
                    Err(e) => {
                        log!("Failed to fetch channels: {}", e);
                        channels_signal.set(Some(Err(e)));
                    }
                }
            });
        }
    });
    // Create a listener for the "someone-joined-audio-channel" event
    create_listener(
        "someone-joined-audio-channel",
        move |data: AudioChannelMemberUpdate| {
            log!(
                "User {} joined audio channel {} on server {}",
                data.user.username,
                data.channel.id,
                data.channel.server_id
            );
            if active_server
                .get()
                .map_or(false, |s| s.id == data.channel.server_id)
            {
                // Update the UI to reflect the new user in the audio channel
                channels_signal.update(|channels| {
                    if let Some(Ok((_, voice_channels))) = channels {
                        voice_channels.iter_mut().find(|c| c.channel.id == data.channel.id).map(|channel| {
                            let loc = channel.users.binary_search_by_key(&data.user.slot, |u| u.slot);
                            match loc {
                                Ok(loc) => {
                                    warn!("A user at the same slot already exists in the channel: {}", data.user.slot);
                                    channel.users[loc] = data.user.clone();
                                }
                                Err(loc) => {
                                    channel.users.insert(loc, data.user.clone());
                                }
                            }
                        });
                    }
                    log!("Updated channels after user joined: {:?}", channels);
                });
            }
        },
    );
    create_listener(
        "someone-left-audio-channel",
        move |data: AudioChannelMemberUpdate| {
            log!(
                "User {} left audio channel {} on server {}",
                data.user.username,
                data.channel.id,
                data.channel.server_id
            );
            if active_server
                .get()
                .map_or(false, |s| s.id == data.channel.server_id)
            {
                // Update the UI to reflect the new user in the audio channel
                channels_signal.update(|channels| {
                    if let Some(Ok((_, voice_channels))) = channels {
                        voice_channels.iter_mut().find(|c| c.channel.id == data.channel.id).map(|channel| {
                            let loc = channel.users.binary_search_by_key(&data.user.slot, |u| u.slot);
                            match loc {
                                Ok(loc) => {
                                    channel.users.remove(loc);
                                }
                                Err(_) => {
                                    warn!("A user does not exist in the given slot in the channel: {}", data.user.slot);
                                }
                            }
                        });
                    }
                    log!("Updated channels after user joined: {:?}", channels);
                });
            }
        },
    );
    view! {
        <div class=style::channel_list_container>
            <Show
                when=move || channels_signal.get().is_some()
                fallback=move || view! { <Loading /> }
            >
                <Show
                    when=move || channels_signal.get().unwrap().is_ok()
                    fallback=move || {
                        view! {
                            <p class="error">
                                "Failed to load channels."
                                {channels_signal.get().unwrap().unwrap_err()}
                            </p>
                        }
                    }
                >
                    {move || {
                        let (text_channels, voice_channels) = channels_signal
                            .get()
                            .unwrap()
                            .unwrap();
                        view! {
                            <ChannelList
                                server_name=active_server
                                text_channels=text_channels
                                voice_channels=voice_channels
                            />
                        }
                    }}
                </Show>
            </Show>
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
            <HoverMenu
                item=move || {
                    view! {
                        <li class=style::channel_list_servername>
                            <h2>
                                {move || {
                                    server_name
                                        .get()
                                        .map_or("No active server".to_string(), |s| s.name.clone())
                                }}
                            </h2>
                        </li>
                    }
                }
                popup=move || {
                    let (is_copied, set_is_copied) = signal(false);
                    view! {
                        <div class=style::channel_list_servername_popup>
                            <p>
                                "Join Channel: "
                                <div
                                    class=move || {
                                        classes!(
                                            style::channel_copiable_text, { if is_copied.get() { Some(style::copied) } else { None } }
                                        )
                                    }
                                    on:click=move |_| {
                                        if let Some(server) = server_name.get() {
                                            let clipboard = window().navigator().clipboard();
                                            let connection_string = server.connection_string.clone();
                                            let _ = clipboard.write_text(connection_string.as_str());
                                            set_is_copied.set(true);
                                            spawn_local(async move {
                                                gloo_timers::future::sleep(Duration::from_millis(500))
                                                    .await;
                                                set_is_copied.set(false);
                                            });
                                        } else {
                                            log!("No active server to copy connection string from.");
                                        }
                                    }
                                >
                                    {move || {
                                        if is_copied.get() {
                                            "Copied!".to_string()
                                        } else {
                                            server_name
                                                .get()
                                                .map_or(
                                                    "No channel".to_string(),
                                                    |c| c.connection_string.clone(),
                                                )
                                        }
                                    }}
                                </div>
                            </p>
                        </div>
                    }
                }
                direction=HoverMenuDirection::Down
                trigger=HoverMenuTrigger::Click
                background_style=vec![HoverMenuBackgroundStyle::Brightness]
            />
            <li
                class=style::channel_list_groupname
                on:click=move |_| {
                    set_show_text_channels.update(|v| *v = !*v);
                }
            >
                <h2>"Text Channels"</h2>
                <div class=move || {
                    if show_text_channels.get() {
                        style::channel_list_down
                    } else {
                        style::channel_list_right
                    }
                }>
                    <h2>"❭"</h2>
                </div>
            </li>
            <Show when=move || show_text_channels.get() fallback=move || view! {}>
                <For
                    each=move || text_channels.get()
                    key=|channel| channel.channel.id
                    children=move |channel| {
                        view! {
                            <li class=style::channel_list_item>
                                <h3>{channel.channel.name}</h3>
                            </li>
                        }
                    }
                />
            </Show>
            <li
                class=style::channel_list_groupname
                on:click=move |_| {
                    set_show_voice_channels.update(|v| *v = !*v);
                }
            >
                <h2>"Voice Channels"</h2>
                <div class=move || {
                    if show_voice_channels.get() {
                        style::channel_list_down
                    } else {
                        style::channel_list_right
                    }
                }>
                    <h2>"❭"</h2>
                </div>
            </li>
            <Show when=move || show_voice_channels.get() fallback=move || view! {}>
                <For
                    each=move || voice_channels.get()
                    key=|channel| channel.channel.id
                    children=move |channel| {
                        view! {
                            <ChannelItem
                                channel=channel.clone()
                                on:click=move |_| {
                                    let channel = channel.clone();
                                    spawn_local(async move {
                                        let channel_name = channel.channel.name.clone();
                                        let join_args = JoinChannel {
                                            channel_with_users: channel,
                                        };
                                        let join_args = serde_wasm_bindgen::to_value(&join_args)
                                            .unwrap();
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
            <h3>{channel.channel.name}</h3>
            <span>{channel.users.len()} " users"</span>
        </li>
        <ul>
            <For
                each=move || channel.users.clone()
                key=|user| user.id
                children=move |user| {
                    view! {
                        <li>
                            <span class=style::channel_user>{user.username.clone()}</span>
                        </li>
                    }
                }
            />
        </ul>
    }
}
