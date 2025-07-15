use front_shared::AudioDevices;
use leptos::{logging::log, prelude::*, task::spawn_local};
use serde_wasm_bindgen::to_value;
use wasm_bindgen::JsValue;

use crate::utils::{dropdown::Dropdown, invoke};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "settings.css"
);

#[derive(Default, Clone, Debug, serde::Deserialize, serde::Serialize)]
struct SetDeviceArgs {
    device: String,
}

#[derive(Default, Clone, Debug, serde::Deserialize, serde::Serialize)]
struct SetBoostArgs {
    boost: i32,
}

#[component]
pub fn Settings() -> impl IntoView {
    let (devices, set_devices) = signal(None::<AudioDevices>);
    spawn_local(async move {
        let device_list = invoke("get_devices", JsValue::NULL).await;
        match device_list {
            Ok(value) => {
                let devices: AudioDevices =
                    serde_wasm_bindgen::from_value(value).unwrap_or_default();
                set_devices.set(Some(devices));
            }
            Err(e) => {
                log!("Failed to fetch devices: {:?}", e);
            }
        }
    });
    view! {
        <div>
            <h2>{"Settings"}</h2>

            // Add your settings UI components here
            <Show when=move || devices.get().is_some()>
                <AudioSettings devices=devices.get().unwrap() />
            </Show>
        </div>
    }
}

#[component]
pub fn AudioSettings(devices: AudioDevices) -> impl IntoView {
    let mic_boost = RwSignal::new(
        devices
            .last_used_devices
            .as_ref()
            .and_then(|d| d.mic_boost)
            .unwrap_or(100)
            .to_string(),
    );
    let speaker_boost = RwSignal::new(
        devices
            .last_used_devices
            .as_ref()
            .and_then(|d| d.speaker_boost)
            .unwrap_or(100)
            .to_string(),
    );

    let set_mic_boost = move || {
        let mic_boost_value = mic_boost.get();
        let mic_boost_value: i32 = mic_boost_value.parse().unwrap_or(100);
        spawn_local(async move {
            if let Err(e) = invoke(
                "set_mic_boost",
                to_value(&SetBoostArgs {
                    boost: mic_boost_value,
                })
                .unwrap(),
            )
            .await
            {
                log!("Failed to set mic boost: {:?}", e);
            }
        });
    };
    let set_speaker_boost = move || {
        let speaker_boost_value = speaker_boost.get();
        let speaker_boost_value: i32 = speaker_boost_value.parse().unwrap_or(100);
        spawn_local(async move {
            if let Err(e) = invoke(
                "set_speaker_boost",
                to_value(&SetBoostArgs {
                    boost: speaker_boost_value,
                })
                .unwrap(),
            )
            .await
            {
                log!("Failed to set speaker boost: {:?}", e);
            }
        });
    };
    let cur_mic = RwSignal::new(devices
        .last_used_devices
        .as_ref()
        .map(|d| d.mic.clone())
        .flatten()
        .unwrap_or("No Microphone Selected".to_string()));
    let cur_speaker = RwSignal::new(devices
        .last_used_devices
        .as_ref()
        .map(|d| d.speaker.clone())
        .flatten()
        .unwrap_or("No Speaker Selected".to_string()));
    view! {
        <h3>{"Audio Settings"}</h3>
        <div class=style::audio_settings>
            <div class=style::audio_setting_type>
                <p>Input Device:</p>
                <Dropdown
                    item=move || { cur_mic.get() }
                    drop_list=move || devices.mics.clone()
                    callback=move |mic| {
                        spawn_local(async move {
                            if let Err(e) = invoke(
                                    "set_mic",
                                    to_value(&SetDeviceArgs { device: mic.clone() }).unwrap(),
                                )
                                .await
                            {
                                log!("Failed to set mic: {:?}", e);
                            } else {
                                cur_mic.set(mic);
                            }
                        });
                    }
                />
                <input
                    type="range"
                    name="mic_boost"
                    id="mic_boost"
                    min="0"
                    max="200"
                    class=style::slider
                    bind:value=mic_boost
                    list="volume_markers"
                    on:change=move |_| set_mic_boost()
                />
            </div>
            <div class=style::audio_setting_type>
                <p>Output Device:</p>
                <Dropdown
                    item=move || { cur_speaker.get() }
                    drop_list=move || devices.speakers.clone()
                    callback=move |speaker| {
                        spawn_local(async move {
                            if let Err(e) = invoke(
                                    "set_speaker",
                                    to_value(&SetDeviceArgs { device: speaker.clone() }).unwrap(),
                                )
                                .await
                            {
                                log!("Failed to set speaker: {:?}", e);
                            } else {
                                cur_speaker.set(speaker);
                            }
                        });
                    }
                />
                <input
                    type="range"
                    name="speaker_boost"
                    id="speaker_boost"
                    min="0"
                    max="200"
                    class=style::slider
                    bind:value=speaker_boost
                    list="volume_markers"
                    on:change=move |_| set_speaker_boost()
                />
            </div>
        </div>

        <datalist id="volume_markers">
            <option value="0"></option>
            <option value="50"></option>
            <option value="100"></option>
            <option value="150"></option>
            <option value="200"></option>
        </datalist>
    }
}
