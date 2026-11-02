//! Browser voice backend for `AudioControl` in the web client. It implements
//! the native audio/voice commands a browser can honour, with the same
//! argument and reply JSON, using the browser's own getUserMedia/WebRTC and
//! WebAudio against the Rust SFU. Native-only commands report "Available in
//! the desktop app". Preferences (not secrets) persist in localStorage.
mod call;
mod device_test;
mod media;
mod socket;
mod soundboard;
pub mod video;

use call::Call;
use device_test::DeviceTest;
use leptos::prelude::*;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc};
use thiscord_frontend::{
    browser_audio::{RemoteSource, VolumeStore},
    browser_call as rules,
    web_shortcuts::Bindings,
};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, SoundId,
    audio::{
        AudioSettings, AudioShortcutStatus, AudioShortcuts, AudioStatus, AudioToggle,
        SpeakerVolumeTarget, StreamLevel,
    },
    voice::VoiceStatus,
};
use wasm_bindgen::{JsCast, prelude::Closure};

const SETTINGS_KEY: &str = "thiscord.browser-audio.v1";
const VOLUMES_KEY: &str = "thiscord.browser-volumes.v1";
const SHORTCUTS_KEY: &str = "thiscord.browser-shortcuts.v1";
const DESKTOP_ONLY: &str = "Available in the desktop app";

#[derive(Default)]
struct State {
    settings: Option<AudioSettings>,
    volumes: Option<VolumeStore>,
    shortcuts: Option<(AudioShortcuts, Bindings)>,
    voice: VoiceStatus,
    call: Option<Rc<Call>>,
    test: Option<Rc<DeviceTest>>,
    /// Playback prepared inside a join or device-test gesture, consumed by
    /// the next `voice_join` or `audio_test`.
    primed: Option<media::Output>,
    pressed: bool,
    /// Call and device-test generations.
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
/// Whether this browser can choose the speaker/headphone device.
pub fn output_selectable() -> bool {
    media::output_selectable()
}

/// Call synchronously from the join or device-test click: mobile browsers
/// only allow audio playback (and start an AudioContext) inside a gesture.
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
/// The "Voice connected" tooltip: RTT, jitter and loss from `getStats`.
pub fn connection_brief() -> String {
    let stats = current().and_then(|call| call.stats.get());
    thiscord_frontend::browser_audio::connection_brief(stats.as_ref())
}
/// Focused-page mute/deafen bindings from Settings → Keyboard shortcuts.
pub fn shortcut_bindings() -> Bindings {
    shortcuts().1
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
fn with_volumes<T>(f: impl FnOnce(&mut VolumeStore) -> T) -> T {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let volumes = state.volumes.get_or_insert_with(|| {
            let stored = storage().and_then(|s| s.get_item(VOLUMES_KEY).ok().flatten());
            VolumeStore::parse(stored.as_deref())
        });
        f(volumes)
    })
}
/// The saved voice or stream-audio level for one person in one guild (1.0
/// when unset). Voice and stream audio are stored separately.
fn stream_gain(guild_id: GuildId, account_id: AccountId, source: RemoteSource) -> f32 {
    let key = rules::volume_key(&SpeakerVolumeTarget {
        guild_id,
        account_id,
        shared_audio: source == RemoteSource::SystemAudio,
    });
    with_volumes(|volumes| volumes.get(&key).unwrap_or(1.0))
}
fn store_gain(target: &SpeakerVolumeTarget, gain: f32) {
    let key = rules::volume_key(target);
    let text = with_volumes(|volumes| {
        volumes.set(key, gain);
        volumes.to_json()
    });
    if let Some(storage) = storage() {
        let _ = storage.set_item(VOLUMES_KEY, &text);
    }
}
fn shortcuts() -> (AudioShortcuts, Bindings) {
    if let Some(saved) = STATE.with(|state| state.borrow().shortcuts.clone()) {
        return saved;
    }
    let saved = storage()
        .and_then(|s| s.get_item(SHORTCUTS_KEY).ok().flatten())
        .and_then(|text| serde_json::from_str::<AudioShortcuts>(&text).ok())
        .and_then(|shortcuts| {
            let bindings = Bindings::from_settings(&shortcuts).ok()?;
            Some((shortcuts, bindings))
        })
        .unwrap_or_else(|| {
            let defaults = AudioShortcuts::default();
            let bindings = Bindings::from_settings(&defaults).unwrap_or_default();
            (defaults, bindings)
        });
    STATE.with(|state| state.borrow_mut().shortcuts = Some(saved.clone()));
    saved
}
fn save_shortcuts(shortcuts: AudioShortcuts) -> Result<(), String> {
    let bindings = Bindings::from_settings(&shortcuts)?;
    let text = serde_json::to_string(&shortcuts).map_err(|_| "Cannot save shortcuts")?;
    storage()
        .and_then(|s| s.set_item(SHORTCUTS_KEY, &text).ok())
        .ok_or("Cannot save shortcuts")?;
    STATE.with(|state| state.borrow_mut().shortcuts = Some((shortcuts, bindings)));
    Ok(())
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
fn stop_test() {
    if let Some(test) = STATE.with(|state| state.borrow_mut().test.take()) {
        test.stop();
    }
}

/// Window listeners, installed once: push-to-talk release on blur, playback
/// recovery after a background tab, and Leave when the page goes away. A
/// hidden tab keeps its call; only `pagehide` ends it. Keyboard shortcuts
/// are handled by the app while the page has focus.
fn install_listeners() {
    if STATE.with(|state| std::mem::replace(&mut state.borrow_mut().listening, true)) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let blur = Closure::<dyn FnMut()>::new(move || pressed(false));
    let visible = Closure::<dyn FnMut()>::new(move || {
        let shown = web_sys::window()
            .and_then(|w| w.document())
            .is_some_and(|d| !d.hidden());
        if shown && let Some(call) = current() {
            // Mobile browsers suspend or interrupt background audio.
            call.output.unlock();
            call.apply();
        } else if !shown {
            pressed(false);
        }
    });
    let hide = Closure::<dyn FnMut()>::new(move || {
        stop_test();
        leave("Disconnected from voice");
    });
    let _ = window.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());
    let _ = window.add_event_listener_with_callback("pagehide", hide.as_ref().unchecked_ref());
    if let Some(document) = window.document() {
        let _ = document
            .add_event_listener_with_callback("visibilitychange", visible.as_ref().unchecked_ref());
    }
    // Page-lifetime listeners.
    blur.forget();
    visible.forget();
    hide.forget();
}

fn test_status() -> Option<AudioStatus> {
    let test = STATE.with(|state| state.borrow().test.clone())?;
    if test.expired() {
        stop_test();
        return None;
    }
    let microphone = test.microphone.as_ref();
    Some(AudioStatus {
        ui_performance: None,
        quality: None,
        recording: None,
        running: true,
        input_level: microphone.and_then(|m| m.level()).unwrap_or(0.0),
        raw_input_level: microphone.and_then(|m| m.raw_level()).unwrap_or(0.0),
        processing_resets: 0,
        echo: None,
        transmitting: false,
        message: if microphone.is_some() {
            "Testing microphone"
        } else {
            "Testing playback"
        }
        .into(),
        streams: vec![],
        dropped_samples: 0,
        underrun_samples: 0,
    })
}

fn audio_status() -> AudioStatus {
    if let Some(status) = test_status() {
        return status;
    }
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
                        let mut sources = vec![RemoteSource::Microphone];
                        if member.sharing_audio {
                            sources.push(RemoteSource::SystemAudio);
                        }
                        sources.into_iter().filter_map(|source| {
                            let slot = source.slot(member.slot)?;
                            let shared_audio = source == RemoteSource::SystemAudio;
                            Some(StreamLevel {
                                speaking: !shared_audio && call.remote_speaking(slot),
                                id: slot.to_string(),
                                target: Some(SpeakerVolumeTarget {
                                    guild_id: call.guild_id,
                                    account_id: member.account_id,
                                    shared_audio,
                                }),
                                label: member.display_name().to_owned(),
                                volume: stream_gain(call.guild_id, member.account_id, source),
                            })
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let level = call.as_ref().map_or(0.0, |c| c.local_level.get());
    let raw = call
        .as_ref()
        .and_then(|c| c.microphone.borrow().as_ref().and_then(|m| m.raw_level()))
        .unwrap_or(level);
    AudioStatus {
        ui_performance: None,
        quality: call
            .as_ref()
            .filter(|_| connected)
            .and_then(|c| c.stats.get())
            .map(|stats| stats.quality()),
        recording: None,
        running: connected,
        input_level: level,
        raw_input_level: raw,
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
    if let Some(test) = STATE.with(|state| state.borrow().test.clone()) {
        if previous.output != next.output {
            test.output
                .set_sink(next.output.as_deref().unwrap_or_default());
        }
        test.apply(next);
    }
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
    stop_test();
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
    output.set_master(settings.output_volume, settings.deafened);
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

async fn audio_test(args: &Value) -> Result<Value, String> {
    let mut requested: AudioSettings = parse(args, "settings")?;
    requested.validate().map_err(str::to_owned)?;
    let microphone: bool = parse(args, "microphone")?;
    let loopback = args
        .get("loopback")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if current().is_some() {
        return Err("Leave voice to test your devices".into());
    }
    stop_test();
    let saved = settings();
    requested.muted = saved.muted;
    requested.deafened = saved.deafened;
    let (id, output) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.next_call += 1;
        (state.next_call, state.primed.take())
    });
    let output = output.unwrap_or_else(media::Output::new);
    let test = DeviceTest::start(id, output, &requested, microphone, loopback).await?;
    let current_test = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let current = state.next_call == id && state.call.is_none();
        if current {
            state.test = Some(Rc::new(test));
            None
        } else {
            Some(test)
        }
    });
    if let Some(stale) = current_test {
        stale.stop();
        return serde_json::to_value(audio_status()).map_err(|_| "Invalid audio response".into());
    }
    let length = STATE
        .with(|state| state.borrow().test.as_ref().map(|t| t.length_ms()))
        .unwrap_or(0.0);
    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(length as u32 + 100).await;
        if STATE.with(|state| state.borrow().test.as_ref().is_some_and(|t| t.id == id)) {
            stop_test();
        }
    });
    serde_json::to_value(audio_status()).map_err(|_| "Invalid audio response".into())
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
        "audio_status" => value(serde_json::to_value(audio_status())),
        "audio_stop" => {
            stop_test();
            value(serde_json::to_value(audio_status()))
        }
        "audio_test" => audio_test(&args).await,
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
            let previous = settings();
            let mut next = previous.clone();
            action.apply(&mut next);
            store_settings(next.clone());
            apply_settings(&previous, &next);
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
        "audio_shortcuts" => value(serde_json::to_value(AudioShortcutStatus {
            shortcuts: shortcuts().0,
            error: None,
        })),
        "audio_shortcuts_save" => {
            save_shortcuts(parse(&args, "settings")?)?;
            Ok(Value::Null)
        }
        "voice_soundboard_play" => {
            let guild_id: GuildId = parse(&args, "guildId")?;
            let channel_id: ChannelId = parse(&args, "channelId")?;
            let sound_id: SoundId = parse(&args, "soundId")?;
            let call = current().ok_or("Join this voice channel to use its soundboard")?;
            soundboard::play(&call, guild_id, channel_id, sound_id)?;
            Ok(Value::Null)
        }
        "soundboard_volume" => {
            let account: AccountId = parse(&args, "accountId")?;
            value(serde_json::to_value(soundboard::volume(account)))
        }
        "soundboard_set_volume" => {
            let account: AccountId = parse(&args, "accountId")?;
            let volume: u16 = parse(&args, "volume")?;
            value(serde_json::to_value(soundboard::set_volume(
                account, volume,
            )?))
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
