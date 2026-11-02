//! Browser voice backend for `AudioControl` in the web client. It implements
//! the subset of the native audio/voice commands a browser can honour, with the
//! same argument and reply JSON, using the browser's own getUserMedia/WebRTC
//! against the Rust SFU. Native-only commands report "Available in the desktop
//! app". Preferences (not secrets) persist in localStorage.
mod call;
mod media;
mod socket;
pub mod video;

use call::Call;
use leptos::prelude::*;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc};
use thiscord_frontend::browser_call::{self as rules, MAX_STORED_VOLUMES};
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    audio::{
        AudioSettings, AudioStatus, AudioToggle, SpeakerVolumeTarget, StreamLevel, TransmitMode,
    },
    voice::{ROOM_CAPACITY, VoiceStatus},
};
use wasm_bindgen::{JsCast, prelude::Closure};

const SETTINGS_KEY: &str = "thiscord.browser-audio.v1";
const VOLUMES_KEY: &str = "thiscord.browser-volumes.v1";
const DESKTOP_ONLY: &str = "Available in the desktop app";

#[derive(Default)]
struct State {
    settings: Option<AudioSettings>,
    volumes: Option<Vec<(String, f32)>>,
    voice: VoiceStatus,
    call: Option<Rc<Call>>,
    /// Playback prepared inside the join gesture, consumed by `voice_join`.
    primed: Option<media::Output>,
    pressed: bool,
    next_call: u64,
    listening: bool,
}
thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static BLOCKED: ArcRwSignal<bool> = ArcRwSignal::new(false);
    static NOTICE: ArcRwSignal<String> = ArcRwSignal::new(String::new());
}

/// Autoplay refused remote audio; the UI offers a tap-to-enable control.
pub fn playback_blocked() -> bool {
    BLOCKED.with(|blocked| blocked.get())
}
fn set_blocked(value: bool) {
    BLOCKED.with(|blocked| {
        if blocked.get_untracked() != value {
            blocked.set(value);
        }
    });
}
/// A browser-specific call notice, such as a denied microphone.
pub fn notice() -> String {
    NOTICE.with(|notice| notice.get())
}
fn set_notice(message: &str) {
    NOTICE.with(|notice| notice.set(message.to_owned()));
}
pub fn output_selectable() -> bool {
    media::output_selectable()
}

/// Call synchronously from the join click: mobile browsers only allow audio
/// playback (and start an AudioContext) inside a user gesture.
pub fn prime() {
    install_listeners();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.primed.is_none() {
            state.primed = Some(media::Output::new());
        }
    });
}
/// Retries refused playback; call from a click handler.
pub fn unlock() {
    if let Some(call) = current() {
        call.output.unlock();
    } else {
        set_blocked(false);
    }
}
/// Push-to-talk from the hold button or the focused-page shortcut.
pub fn pressed(pressed: bool) {
    let changed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        std::mem::replace(&mut state.pressed, pressed) != pressed
    });
    if changed && let Some(call) = current() {
        call.apply();
    }
}

fn current() -> Option<Rc<Call>> {
    STATE.with(|state| state.borrow().call.clone())
}
fn pressed_now() -> bool {
    STATE.with(|state| state.borrow().pressed)
}
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
fn settings() -> AudioSettings {
    if let Some(settings) = STATE.with(|state| state.borrow().settings.clone()) {
        return settings;
    }
    let settings = storage()
        .and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten())
        .and_then(|text| serde_json::from_str::<AudioSettings>(&text).ok())
        .filter(|settings| settings.validate().is_ok())
        .unwrap_or_else(rules::browser_defaults);
    STATE.with(|state| state.borrow_mut().settings = Some(settings.clone()));
    settings
}
fn store_settings(settings: AudioSettings) {
    if let (Some(storage), Ok(text)) = (storage(), serde_json::to_string(&settings)) {
        let _ = storage.set_item(SETTINGS_KEY, &text);
    }
    STATE.with(|state| state.borrow_mut().settings = Some(settings));
}
fn with_volumes<T>(f: impl FnOnce(&mut Vec<(String, f32)>) -> T) -> T {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let volumes = state.volumes.get_or_insert_with(|| {
            storage()
                .and_then(|s| s.get_item(VOLUMES_KEY).ok().flatten())
                .and_then(|text| serde_json::from_str::<Vec<(String, f32)>>(&text).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|(key, gain)| {
                    key.len() < 128 && gain.is_finite() && (0.0..=2.0).contains(gain)
                })
                .take(MAX_STORED_VOLUMES)
                .collect()
        });
        f(volumes)
    })
}
fn stream_gain(guild_id: GuildId, account_id: AccountId, shared_audio: bool) -> f32 {
    let key = rules::volume_key(&SpeakerVolumeTarget {
        guild_id,
        account_id,
        shared_audio,
    });
    with_volumes(|volumes| {
        volumes
            .iter()
            .find(|(stored, _)| *stored == key)
            .map_or(1.0, |(_, gain)| *gain)
    })
}
fn store_gain(target: &SpeakerVolumeTarget, gain: f32) {
    let key = rules::volume_key(target);
    let text = with_volumes(|volumes| {
        volumes.retain(|(stored, _)| *stored != key);
        volumes.push((key, gain));
        let excess = volumes.len().saturating_sub(MAX_STORED_VOLUMES);
        volumes.drain(..excess);
        serde_json::to_string(volumes).ok()
    });
    if let (Some(storage), Some(text)) = (storage(), text) {
        let _ = storage.set_item(VOLUMES_KEY, &text);
    }
}

fn update_voice(call: &Call, f: impl FnOnce(&mut VoiceStatus)) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.call.as_ref().is_some_and(|c| c.id == call.id) {
            f(&mut state.voice);
        }
    });
}
/// Ends a call that will not retry, keeping its message for the UI.
fn finish(call: &Call, message: String) {
    let owned = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.call.as_ref().is_some_and(|c| c.id == call.id) {
            return false;
        }
        state.call = None;
        state.pressed = false;
        state.voice = VoiceStatus {
            message,
            ..Default::default()
        };
        true
    });
    if owned {
        set_blocked(false);
    }
    call.shutdown();
}
fn leave(message: &str) {
    let call = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.pressed = false;
        state.voice = VoiceStatus {
            message: message.into(),
            ..Default::default()
        };
        state.call.take()
    });
    if let Some(call) = call {
        call.shutdown();
    }
    set_blocked(false);
    set_notice("");
}

/// Window listeners, installed once: focused-page push-to-talk, playback
/// recovery after a background tab, and Leave when the page goes away. A
/// hidden tab keeps its call; only `pagehide` ends it.
fn install_listeners() {
    if STATE.with(|state| std::mem::replace(&mut state.borrow_mut().listening, true)) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let ptt_mode = || current().is_some() && settings().mode == TransmitMode::PushToTalk;
    let down =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
            if e.code() == "Space" && e.ctrl_key() && e.shift_key() && ptt_mode() {
                e.prevent_default();
                if !e.repeat() {
                    pressed(true);
                }
            }
        });
    let up = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
        if pressed_now()
            && matches!(
                e.code().as_str(),
                "Space" | "ControlLeft" | "ControlRight" | "ShiftLeft" | "ShiftRight"
            )
        {
            pressed(false);
        }
    });
    let blur = Closure::<dyn FnMut()>::new(move || pressed(false));
    let visible = Closure::<dyn FnMut()>::new(move || {
        let shown = web_sys::window()
            .and_then(|w| w.document())
            .is_some_and(|d| !d.hidden());
        if shown && let Some(call) = current() {
            call.output.unlock();
            call.apply();
        } else if !shown {
            pressed(false);
        }
    });
    let hide = Closure::<dyn FnMut()>::new(move || leave("Disconnected from voice"));
    let _ = window.add_event_listener_with_callback("keydown", down.as_ref().unchecked_ref());
    let _ = window.add_event_listener_with_callback("keyup", up.as_ref().unchecked_ref());
    let _ = window.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());
    let _ = window.add_event_listener_with_callback("pagehide", hide.as_ref().unchecked_ref());
    if let Some(document) = window.document() {
        let _ = document
            .add_event_listener_with_callback("visibilitychange", visible.as_ref().unchecked_ref());
    }
    // Page-lifetime listeners.
    down.forget();
    up.forget();
    blur.forget();
    visible.forget();
    hide.forget();
}

fn audio_status() -> AudioStatus {
    let call = current();
    let connected = call.as_ref().is_some_and(|c| c.connected());
    let streams = call
        .as_ref()
        .and_then(|call| {
            let live = call.live.borrow();
            let live = live.as_ref()?;
            let own = STATE.with(|state| state.borrow().voice.own_slot);
            Some(
                live.roster
                    .iter()
                    .filter(|member| Some(member.slot) != own)
                    .flat_map(|member| {
                        let mut streams = vec![(member.slot, false)];
                        if member.sharing_audio {
                            streams.push((ROOM_CAPACITY + member.slot, true));
                        }
                        streams.into_iter().map(|(slot, shared_audio)| {
                            let target = SpeakerVolumeTarget {
                                guild_id: call.guild_id,
                                account_id: member.account_id,
                                shared_audio,
                            };
                            StreamLevel {
                                speaking: !shared_audio && call.remote_speaking(slot),
                                id: slot.to_string(),
                                target: Some(target),
                                label: member.display_name().to_owned(),
                                volume: stream_gain(call.guild_id, member.account_id, shared_audio),
                            }
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let level = call.as_ref().map_or(0.0, |c| c.local_level.get());
    AudioStatus {
        ui_performance: None,
        quality: None,
        recording: None,
        running: connected,
        input_level: level,
        raw_input_level: level,
        processing_resets: 0,
        echo: None,
        transmitting: connected && call.as_ref().is_some_and(|c| c.transmitting.get()),
        message: if call.is_some() {
            "Browser voice".into()
        } else {
            "Voice is idle".into()
        },
        streams,
        dropped_samples: 0,
        underrun_samples: 0,
    }
}

fn apply_settings(previous: &AudioSettings, next: &AudioSettings) {
    let Some(call) = current() else {
        return;
    };
    if previous.output != next.output {
        call.output
            .set_sink(next.output.as_deref().unwrap_or_default());
    }
    if rules::Capture::from_settings(previous) != rules::Capture::from_settings(next) {
        call.reopen_microphone();
    }
    call.apply();
}

async fn voice_join(args: &Value) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Join {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
    }
    let join: Join = serde_json::from_value(args.clone()).map_err(|_| "Invalid voice request")?;
    if let Some(call) = current()
        && call.guild_id == join.guild_id
        && call.channel_id.get() == join.channel_id
    {
        return Ok(Value::Null);
    }
    leave("Disconnected from voice");
    install_listeners();
    // Claim the call slot before awaiting the microphone so a second click or
    // a leave during the permission prompt cannot start a competing call.
    let id = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.next_call += 1;
        state.voice = VoiceStatus {
            channel_id: Some(join.channel_id),
            message: "Connecting to voice…".into(),
            ..Default::default()
        };
        state.next_call
    });
    let output = STATE
        .with(|state| state.borrow_mut().primed.take())
        .unwrap_or_else(media::Output::new);
    let settings = settings();
    if let Some(device) = &settings.output {
        output.set_sink(device);
    }
    let capture = rules::Capture::from_settings(&settings);
    let microphone = media::Microphone::open(&capture, output.context()).await;
    let still_current = STATE.with(|state| {
        let state = state.borrow();
        state.next_call == id
            && state.call.is_none()
            && state.voice.channel_id == Some(join.channel_id)
    });
    if !still_current {
        if let Ok(microphone) = &microphone {
            microphone.stop();
        }
        output.close();
        return Ok(Value::Null);
    }
    match &microphone {
        Ok(_) => set_notice(""),
        Err(error) => set_notice(error.notice()),
    }
    let call = Call::new(
        id,
        join.guild_id,
        join.channel_id,
        join.token,
        output,
        microphone.ok(),
    );
    STATE.with(|state| state.borrow_mut().call = Some(call.clone()));
    call.start();
    Ok(Value::Null)
}

fn parse<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|_| "Invalid audio request".into())
}

async fn dispatch(name: &str, args: Value) -> Result<Value, String> {
    let value =
        |v: Result<Value, serde_json::Error>| v.map_err(|_| "Invalid audio response".to_owned());
    match name {
        "voice_join" => voice_join(&args).await,
        "voice_leave" => {
            leave("Disconnected from voice");
            Ok(Value::Null)
        }
        "voice_status" => value(serde_json::to_value(
            STATE.with(|state| state.borrow().voice.clone()),
        )),
        "audio_status" | "audio_stop" => value(serde_json::to_value(audio_status())),
        "audio_load" | "audio_current" => value(serde_json::to_value(settings())),
        "audio_save" => {
            let next: AudioSettings = parse(&args, "settings")?;
            next.validate().map_err(str::to_owned)?;
            let previous = settings();
            store_settings(next.clone());
            apply_settings(&previous, &next);
            Ok(Value::Null)
        }
        "audio_toggle" => {
            let action: AudioToggle = parse(&args, "action")?;
            let mut next = settings();
            action.apply(&mut next);
            store_settings(next);
            if let Some(call) = current() {
                call.apply();
            }
            Ok(Value::Null)
        }
        "audio_pressed" => {
            pressed(parse(&args, "pressed")?);
            value(serde_json::to_value(audio_status()))
        }
        "audio_devices" => value(serde_json::to_value(media::devices().await?)),
        "audio_volume" => {
            let gain: f32 = parse(&args, "gain")?;
            let target: Option<SpeakerVolumeTarget> = parse(&args, "target")?;
            if !gain.is_finite() || !(0.0..=2.0).contains(&gain) {
                return Err("Invalid volume".into());
            }
            if let Some(target) = target {
                store_gain(&target, gain);
            }
            if let Some(call) = current() {
                call.apply();
            }
            value(serde_json::to_value(audio_status()))
        }
        _ => Err(DESKTOP_ONLY.into()),
    }
}

pub async fn invoke<T: serde::de::DeserializeOwned>(name: &str, args: Value) -> Result<T, String> {
    let reply = dispatch(name, args).await?;
    serde_json::from_value(reply).map_err(|_| "Invalid audio response".into())
}

/// Resolves `future`, or `None` after `timeout_ms`.
async fn within<T>(timeout_ms: u32, future: impl std::future::Future<Output = T>) -> Option<T> {
    match futures_util::future::select(
        Box::pin(future),
        gloo_timers::future::TimeoutFuture::new(timeout_ms),
    )
    .await
    {
        futures_util::future::Either::Left((value, _)) => Some(value),
        futures_util::future::Either::Right(_) => None,
    }
}
