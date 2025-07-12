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
    view! {
        <div>
            <h2>{"Settings"}</h2>
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
                </div>
            </div>
        </div>
    }
}
