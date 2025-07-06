use front_shared::{LoginStatus, Status};
use gloo_timers::future::sleep;
use leptos::{context, logging::log, prelude::*, task::spawn_local};
use stylance::classes;
use wasm_bindgen::JsValue;

use crate::{
    app::LoggedInSignal,
    utils::{create_listener, invoke},
};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "status.css"
);

fn get_status(set_status: WriteSignal<Status>) {
    spawn_local(async move {
        let initial_status = invoke("get_status", JsValue::NULL).await;
        match initial_status {
            Ok(value) => {
                let status: Status = serde_wasm_bindgen::from_value(value).unwrap_or(Status::Offline);
                set_status.set(status);
            }
            Err(e) => {
                log!("Failed to fetch initial status: {:?}", e);
                set_status.set(Status::Offline);
            }
        }
    });
}

#[component]
pub fn StatusBox() -> impl IntoView {
    let session = context::use_context::<LoggedInSignal>().expect("Session context not found");
    let username = move || {
        if let LoginStatus::LoggedIn(user) = session.get() {
            user.username
        } else {
            "Guest".into()
        }
    };
    let (status, set_status) = signal(Status::Offline);
    let (mic_muted, set_mic_muted) = signal(false);
    let (speaker_muted, set_speaker_muted) = signal(false);

    get_status(set_status);

    create_listener("status_change", move |new_status: Status| {
        if matches!(new_status, Status::OnCall(_)) {
            set_mic_muted.set(false);
            set_speaker_muted.set(false);
        }
        set_status.set(new_status);
    });

    view! {
        <div class=style::status_box>
            <span class=style::username>{username}</span>
            <div class=style::call_status>
                <span class=style::status>{move || format!("{:?}", status.get())}</span>
                <Show when=move || matches!(status.get(), Status::OnCall(_)) fallback=move || {}>
                    <div class=style::call_container>
                        <div class=style::icon_div>
                            <img
                                class=classes!(
                                    style::icon, if mic_muted.get() {Some(style::mkred)} else {None}
                                )
                                src=move || {
                                    if mic_muted.get() {
                                        "public/mic_off.png"
                                    } else {
                                        "public/mic_on.png"
                                    }
                                }
                                on:click=move |_| {
                                    spawn_local(async move {
                                        let new_mic_muted = !mic_muted.get();
                                        if new_mic_muted {
                                            let res = invoke("mute_microphone", JsValue::NULL).await;
                                            if let Err(e) = res {
                                                log!("Failed to mute microphone: {:?}", e);
                                            } else {
                                                set_mic_muted.set(true);
                                            }
                                        } else {
                                            let res = invoke("unmute_microphone", JsValue::NULL).await;
                                            if let Err(e) = res {
                                                log!("Failed to unmute microphone: {:?}", e);
                                            } else {
                                                set_mic_muted.set(false);
                                            }
                                        }
                                    });
                                }
                            />
                        </div>
                        <div class=style::icon_div>
                            <img
                                class=classes!(
                                    style::icon, if speaker_muted.get() {Some(style::mkred)} else {None}
                                )
                                src=move || {
                                    if speaker_muted.get() {
                                        "public/speaker_off.png"
                                    } else {
                                        "public/speaker_on.png"
                                    }
                                }
                                on:click=move |_| {
                                    spawn_local(async move {
                                        let new_speaker_muted = !speaker_muted.get();
                                        if new_speaker_muted {
                                            let res = invoke("deafen_speaker", JsValue::NULL).await;
                                            if let Err(e) = res {
                                                log!("Failed to deafen speaker: {:?}", e);
                                            } else {
                                                set_speaker_muted.set(true);
                                            }
                                        } else {
                                            let res = invoke("undeafen_speaker", JsValue::NULL).await;
                                            if let Err(e) = res {
                                                log!("Failed to undeafen speaker: {:?}", e);
                                            } else {
                                                set_speaker_muted.set(false);
                                            }
                                        }
                                    });
                                }
                            />
                        </div>
                        <div class=style::icon_div>
                            <img
                                class=classes!(style::icon, style::mkred)
                                src="public/disconnect_call.png"
                                on:click=move |_| {
                                    spawn_local(async move {
                                        let res = invoke("disconnect_call", JsValue::NULL).await;
                                        if let Err(e) = res {
                                            log!("Failed to end call: {:?}", e);
                                        } else {
                                            sleep(std::time::Duration::from_millis(100)).await;
                                            get_status(set_status);
                                            set_mic_muted.set(false);
                                            set_speaker_muted.set(false);
                                        }
                                    });
                                }
                            />
                        </div>
                    </div>
                </Show>
            </div>
        </div>
    }
}
