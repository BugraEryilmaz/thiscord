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
    let mic_boost = RwSignal::new("100".to_string());
    let speaker_boost = RwSignal::new("100".to_string());
    Effect::new(move || {
        let mic_boost_value = mic_boost.get();
        let mic_boost_value: i32 = mic_boost_value.parse().unwrap_or(100);
        spawn_local(async move {
            if let Err(e) = invoke(
                "set_mic_boost",
                to_value(&SetBoostArgs { boost: mic_boost_value }).unwrap(),
            )
            .await
            {
                log!("Failed to set mic boost: {:?}", e);
            }
        });
    });
    Effect::new(move || {
        let speaker_boost_value = speaker_boost.get();
        let speaker_boost_value: i32 = speaker_boost_value.parse().unwrap_or(100);
        spawn_local(async move {
            if let Err(e) = invoke(
                "set_speaker_boost",
                to_value(&SetBoostArgs { boost: speaker_boost_value }).unwrap(),
            )
            .await
            {
                log!("Failed to set speaker boost: {:?}", e);
            }
        });
    });
    view! {
        <div>
            <h2>{"Settings"}</h2>
            <datalist id="volume_markers">
                <option value="0"></option>
                <option value="50"></option>
                <option value="100"></option>
                <option value="150"></option>
                <option value="200"></option>
            </datalist>

            // Add your settings UI components here

            <div class=style::audio_settings>
                <div class=style::audio_setting_type>
                    <p>Input Device:</p>
                    <Dropdown
                        item=move || {
                            devices
                                .get()
                                .last_used_devices
                                .map(|d| {
                                    d.mic.clone().unwrap_or("No Microphone Selected".to_string())
                                })
                                .unwrap_or("No Microphone Selected".to_string())
                        }
                        drop_list=move || devices.get().mics.clone()
                        callback=move |mic| {
                            spawn_local(async move {
                                let _ = invoke(
                                        "set_mic",
                                        to_value(&SetDeviceArgs { device: mic }).unwrap(),
                                    )
                                    .await;
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
                    />
                </div>
                <div class=style::audio_setting_type>
                    <p>Output Device:</p>
                    <Dropdown
                        item=move || {
                            devices
                                .get()
                                .last_used_devices
                                .map(|d| {
                                    d.speaker.clone().unwrap_or("No Speaker Selected".to_string())
                                })
                                .unwrap_or("No Speaker Selected".to_string())
                        }
                        drop_list=move || devices.get().speakers.clone()
                        callback=move |speaker| {
                            spawn_local(async move {
                                let _ = invoke(
                                        "set_speaker",
                                        to_value(&SetDeviceArgs { device: speaker }).unwrap(),
                                    )
                                    .await;
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
                    />
                </div>
            </div>
        </div>
    }
}
