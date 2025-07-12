use front_shared::AudioDevices;
use leptos::{logging::log, prelude::*, task::spawn_local};
use serde_wasm_bindgen::to_value;
use wasm_bindgen::JsValue;

use crate::utils::{
    hover_menu::{HoverMenu, HoverMenuDirection, HoverMenuTrigger},
    invoke,
};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "settings.css"
);

#[derive(Default, Clone, Debug, serde::Deserialize, serde::Serialize)]
struct SetDeviceArgs {
    device: String,
}

#[component]
pub fn Settings() -> impl IntoView {
    let (devices, set_devices) = signal(AudioDevices::default());
    spawn_local(async move {
        let device_list = invoke("get_devices", JsValue::NULL).await;
        match device_list {
            Ok(value) => {
                let devices: AudioDevices =
                    serde_wasm_bindgen::from_value(value).unwrap_or_default();
                set_devices.set(devices);
            }
            Err(e) => {
                log!("Failed to fetch devices: {:?}", e);
            }
        }
    });
    let mic_popup_visible = RwSignal::new(false);
    let speaker_popup_visible = RwSignal::new(false);
    view! {
        <div>
            <h2>{"Settings"}</h2>
            // Add your settings UI components here

            <div class=style::audio_settings>
                <div class=style::audio_setting_type>
                    <p>Input Device:</p>
                    <HoverMenu
                        item=move || {
                            view! {
                                <p class=style::audio_setting_current>
                                    {devices
                                        .get()
                                        .last_used_devices
                                        .map(|d| d.mic.clone().unwrap_or("No Microphone Selected".to_string()))
                                        .unwrap_or("No Microphone Selected".to_string())}
                                </p>
                            }
                        }
                        popup={
                            view! {
                                <div class=style::audio_setting_popup>
                                    <For
                                        each=move || devices.get().mics.into_iter()
                                        key=|mic| mic.clone()
                                        let(mic)
                                    >
                                        <p
                                            class=style::audio_setting_option
                                            on:click=move |_| {
                                                mic_popup_visible.set(false);
                                                let mic = mic.clone();
                                                spawn_local(async move {
                                                    let _ = invoke(
                                                            "set_mic",
                                                            to_value(&SetDeviceArgs { device: mic }).unwrap(),
                                                        )
                                                        .await;
                                                });
                                            }
                                        >
                                            {mic.clone()}
                                        </p>
                                    </For>
                                </div>
                            }
                        }
                        direction=HoverMenuDirection::Down
                        trigger=HoverMenuTrigger::Click
                        visible=mic_popup_visible
                    />
                </div>
                <div class=style::audio_setting_type>
                    <p>Output Device:</p>
                    <HoverMenu
                        item=move || {
                            view! {
                                <p class=style::audio_setting_current>
                                    {devices
                                        .get()
                                        .last_used_devices
                                        .map(|d| d.speaker.clone().unwrap_or("No Speaker Selected".to_string()))
                                        .unwrap_or("No Speaker Selected".to_string())}
                                </p>
                            }
                        }
                        popup={
                            view! {
                                <div class=style::audio_setting_popup>
                                    <For
                                        each=move || devices.get().speakers.into_iter()
                                        key=|speaker| speaker.clone()
                                        let(speaker)
                                    >
                                        <p
                                            class=style::audio_setting_option
                                            on:click=move |_| {
                                                speaker_popup_visible.set(false);
                                                let speaker = speaker.clone();
                                                spawn_local(async move {
                                                    let _ = invoke(
                                                            "set_speaker",
                                                            to_value(&SetDeviceArgs { device: speaker }).unwrap(),
                                                        )
                                                        .await;
                                                });
                                            }
                                        >
                                            {speaker.clone()}
                                        </p>
                                    </For>
                                </div>
                            }
                        }
                        direction=HoverMenuDirection::Down
                        trigger=HoverMenuTrigger::Click
                        visible=speaker_popup_visible
                    />
                </div>
            </div>
        </div>
    }
}
