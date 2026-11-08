//! Voice messages: the inline player and the composer recorder.
//!
//! The desktop records, uploads, downloads and plays natively on the selected
//! devices (`native_voice_message.rs`); only levels and positions reach the
//! WebView. The browser client records with MediaRecorder (Ogg or WebM Opus,
//! normalized to Ogg in Rust), uploads through the bounded transfer helper and
//! plays through one shared media element on the selected output. One voice
//! message plays at a time in either client.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use crate::account_client as client;
use leptos::prelude::*;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use thiscord_frontend::voice_clip::{self, Gesture};
use thiscord_shared::{
    ChannelId, GuildId,
    chat::{ATTACHMENTS_PATH, ChatAttachment},
    friends::DIRECT_ATTACHMENTS_PATH,
    ids::ConversationId,
    voice_message::{
        MAX_VOICE_MESSAGE_BYTES, MAX_VOICE_MESSAGE_MS, MIN_VOICE_MESSAGE_MS, WAVEFORM_BARS,
        encode_waveform,
    },
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

/// Bars drawn by the inline player and the preview.
const BARS: usize = 44;
/// Bars of the scrolling live waveform while recording.
const LIVE_BARS: usize = 72;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Loading,
    Playing,
    Paused,
    Error,
}

/// The voice message that owns playback (at most one).
#[derive(Clone, PartialEq)]
struct Now {
    key: String,
    state: State,
    position_ms: u32,
    duration_ms: u32,
    error: String,
}

thread_local! {
    static NOW: ArcRwSignal<Option<Now>> = ArcRwSignal::new(None);
    /// Shared speed preference for this session (1×, 1.5×, 2×).
    static SPEED: ArcRwSignal<f32> = ArcRwSignal::new(1.0);
    static NATIVE_PLAYER: RefCell<Option<client::EventListener>> = const { RefCell::new(None) };
    static BROWSER: RefCell<Option<BrowserPlayer>> = const { RefCell::new(None) };
}
fn now() -> ArcRwSignal<Option<Now>> {
    NOW.with(Clone::clone)
}
fn speed() -> ArcRwSignal<f32> {
    SPEED.with(Clone::clone)
}
fn set_state(key: &str, state: State, position_ms: u32, duration_ms: u32, error: String) {
    now().set(Some(Now {
        key: key.to_owned(),
        state,
        position_ms,
        duration_ms,
        error,
    }));
}

/// Stops playback (sign-out, access loss, leaving a conversation's owner).
pub(super) fn stop_all() {
    now().set(None);
    if client::desktop() {
        leptos::task::spawn_local(async {
            let _ = client::native::<()>(
                "voice_message_control",
                serde_json::json!({"control":{"action":"stop"}}),
            )
            .await;
        });
    } else {
        BROWSER.with_borrow_mut(|player| {
            if let Some(player) = player.as_mut() {
                player.stop();
            }
        });
    }
}

#[derive(serde::Deserialize)]
struct NativeStatus {
    key: String,
    state: String,
    position_ms: u32,
    duration_ms: u32,
    #[serde(default)]
    error: Option<String>,
}

fn listen_native_player() {
    if NATIVE_PLAYER.with_borrow(Option::is_some) {
        return;
    }
    leptos::task::spawn_local(async {
        let listener = client::listen("voice-message-player", |status: NativeStatus| {
            let current = now().get_untracked();
            match status.state.as_str() {
                "playing" | "paused" | "loading" => {
                    // Ignore late events for a message that is no longer current.
                    if current.as_ref().is_some_and(|n| n.key == status.key) {
                        let state = match status.state.as_str() {
                            "playing" => State::Playing,
                            "paused" => State::Paused,
                            _ => State::Loading,
                        };
                        set_state(
                            &status.key,
                            state,
                            status.position_ms,
                            status.duration_ms,
                            String::new(),
                        );
                    }
                }
                "ended" | "stopped" => {
                    if current.as_ref().is_some_and(|n| n.key == status.key) {
                        now().set(None);
                    }
                }
                _ => {
                    if current.as_ref().is_some_and(|n| n.key == status.key) {
                        set_state(
                            &status.key,
                            State::Error,
                            status.position_ms,
                            status.duration_ms,
                            status.error.unwrap_or_else(|| "Playback failed".into()),
                        );
                    }
                }
            }
        })
        .await;
        if let Ok(listener) = listener {
            NATIVE_PLAYER.with_borrow_mut(|slot| *slot = Some(listener));
        }
    });
}

/// What to play: a published attachment, or the unsent recording.
#[derive(Clone)]
pub(super) enum Source {
    Attachment {
        attachment: ChatAttachment,
        private: bool,
    },
    Recording {
        /// Browser recordings keep their Ogg bytes in this Blob.
        blob: Option<web_sys::Blob>,
        duration_ms: u32,
    },
}
impl Source {
    fn key(&self) -> String {
        match self {
            Self::Attachment { attachment, .. } => attachment.id.to_string(),
            Self::Recording { .. } => "recording".into(),
        }
    }
    fn duration(&self) -> u32 {
        match self {
            Self::Attachment { attachment, .. } => {
                attachment.voice.as_ref().map_or(0, |v| v.duration_ms)
            }
            Self::Recording { duration_ms, .. } => *duration_ms,
        }
    }
}

fn start(ui: Ui, source: Source, position_ms: u32) {
    let key = source.key();
    let duration = source.duration();
    set_state(&key, State::Loading, position_ms, duration, String::new());
    let rate = speed().get_untracked();
    if client::desktop() {
        listen_native_player();
        let token = ui.token.get_untracked().unwrap_or_default();
        let native_source = match &source {
            Source::Attachment {
                attachment,
                private,
            } => serde_json::json!({"kind":"attachment","id":attachment.id,"private":private}),
            Source::Recording { .. } => serde_json::json!({"kind":"recording"}),
        };
        leptos::task::spawn_local(async move {
            let result = client::native::<()>(
                "voice_message_play",
                serde_json::json!({"token":token,"source":native_source,"positionMs":position_ms,"speed":rate}),
            )
            .await;
            if let Err(error) = result
                && now().get_untracked().is_some_and(|n| n.key == key)
            {
                set_state(&key, State::Error, position_ms, duration, error);
            }
        });
    } else {
        leptos::task::spawn_local(async move {
            if let Err(error) = browser_play(ui, source, position_ms, rate).await
                && now().get_untracked().is_some_and(|n| n.key == key)
            {
                set_state(&key, State::Error, position_ms, duration, error);
            }
        });
    }
}

fn control(action: serde_json::Value) {
    leptos::task::spawn_local(async move {
        let _ = client::native::<()>(
            "voice_message_control",
            serde_json::json!({"control":action}),
        )
        .await;
    });
}

fn toggle(ui: Ui, source: Source) {
    let key = source.key();
    match now().get_untracked() {
        Some(n) if n.key == key && n.state == State::Playing => {
            if client::desktop() {
                control(serde_json::json!({"action":"pause"}));
            } else {
                BROWSER.with_borrow_mut(|p| p.as_mut().map(BrowserPlayer::pause));
            }
            set_state(
                &key,
                State::Paused,
                n.position_ms,
                n.duration_ms,
                String::new(),
            );
        }
        Some(n) if n.key == key && n.state == State::Paused => {
            if client::desktop() {
                control(serde_json::json!({"action":"resume"}));
                set_state(
                    &key,
                    State::Playing,
                    n.position_ms,
                    n.duration_ms,
                    String::new(),
                );
            } else if BROWSER.with_borrow_mut(|p| p.as_mut().is_some_and(BrowserPlayer::resume)) {
                set_state(
                    &key,
                    State::Playing,
                    n.position_ms,
                    n.duration_ms,
                    String::new(),
                );
            } else {
                start(ui, source, n.position_ms);
            }
        }
        Some(n) if n.key == key && n.state == State::Loading => {}
        _ => start(ui, source, 0),
    }
}

fn seek(ui: Ui, source: Source, position_ms: u32) {
    let key = source.key();
    match now().get_untracked() {
        Some(n) if n.key == key && n.state != State::Error => {
            if client::desktop() {
                control(serde_json::json!({"action":"seek","position_ms":position_ms}));
            } else {
                BROWSER.with_borrow_mut(|p| p.as_mut().map(|p| p.seek(position_ms)));
            }
            set_state(&key, n.state, position_ms, n.duration_ms, String::new());
        }
        _ => start(ui, source, position_ms),
    }
}

fn change_speed() {
    let next = voice_clip::next_speed(speed().get_untracked());
    speed().set(next);
    if now().get_untracked().is_some() {
        if client::desktop() {
            control(serde_json::json!({"action":"speed","speed":next}));
        } else {
            BROWSER.with_borrow_mut(|p| {
                p.as_mut()
                    .map(|p| p.element.set_playback_rate(f64::from(next)))
            });
        }
    }
}

// ---- Browser playback ------------------------------------------------------

struct BrowserPlayer {
    element: web_sys::HtmlAudioElement,
    key: String,
    url: Option<String>,
    duration_ms: u32,
    timer: Option<gloo_timers::callback::Interval>,
    _ended: Closure<dyn FnMut()>,
    _failed: Closure<dyn FnMut()>,
}
impl BrowserPlayer {
    fn new() -> Option<Self> {
        let element = web_sys::HtmlAudioElement::new().ok()?;
        element.set_preload("auto");
        let ended = Closure::<dyn FnMut()>::new(|| {
            BROWSER.with_borrow_mut(|p| {
                if let Some(p) = p.as_mut() {
                    p.timer = None;
                }
            });
            now().set(None);
        });
        let failed = Closure::<dyn FnMut()>::new(|| {
            if let Some(n) = now().get_untracked() {
                set_state(
                    &n.key,
                    State::Error,
                    n.position_ms,
                    n.duration_ms,
                    "This browser can't play voice messages".into(),
                );
            }
        });
        element.set_onended(Some(ended.as_ref().unchecked_ref()));
        element.set_onerror(Some(failed.as_ref().unchecked_ref()));
        Some(Self {
            element,
            key: String::new(),
            url: None,
            duration_ms: 0,
            timer: None,
            _ended: ended,
            _failed: failed,
        })
    }
    fn release(&mut self) {
        self.timer = None;
        let _ = self.element.pause();
        if let Some(url) = self.url.take() {
            let _ = self.element.remove_attribute("src");
            self.element.load();
            let _ = web_sys::Url::revoke_object_url(&url);
        }
        self.key.clear();
    }
    fn stop(&mut self) {
        self.release();
    }
    fn pause(&mut self) {
        self.timer = None;
        let _ = self.element.pause();
    }
    fn resume(&mut self) -> bool {
        if self.url.is_none() {
            return false;
        }
        play_element(&self.element);
        self.track();
        true
    }
    fn seek(&mut self, position_ms: u32) {
        self.element
            .set_current_time(f64::from(position_ms) / 1000.0);
    }
    /// Positions update at ~20 Hz while playing (smoother than `timeupdate`).
    fn track(&mut self) {
        let element = self.element.clone();
        let key = self.key.clone();
        let duration = self.duration_ms;
        self.timer = Some(gloo_timers::callback::Interval::new(50, move || {
            if now()
                .get_untracked()
                .is_some_and(|n| n.key == key && n.state == State::Playing)
            {
                let position = (element.current_time() * 1000.0) as u32;
                set_state(
                    &key,
                    State::Playing,
                    position.min(duration),
                    duration,
                    String::new(),
                );
            }
        }));
    }
}

fn play_element(element: &web_sys::HtmlAudioElement) {
    if let Ok(promise) = element.play() {
        leptos::task::spawn_local(async move {
            if JsFuture::from(promise).await.is_err()
                && let Some(n) = now().get_untracked()
                && n.state != State::Paused
            {
                set_state(
                    &n.key,
                    State::Error,
                    n.position_ms,
                    n.duration_ms,
                    "Playback could not start".into(),
                );
            }
        });
    }
}

async fn blob_bytes(blob: &web_sys::Blob) -> Result<Vec<u8>, String> {
    let buffer = JsFuture::from(blob.array_buffer())
        .await
        .map_err(|_| "Cannot read the voice message")?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

fn blob_of(bytes: &[u8], mime: &str) -> Result<web_sys::Blob, String> {
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|_| "Cannot prepare the voice message".into())
}

/// Ogg where the media element supports it, otherwise a WebM copy (Safari).
fn playable(element: &web_sys::HtmlAudioElement, bytes: &[u8]) -> Result<web_sys::Blob, String> {
    let parsed = thiscord_shared::voice_message::parse_ogg_opus(bytes)
        .map_err(|_| "This voice message is damaged".to_owned())?;
    if !element.can_play_type("audio/ogg; codecs=opus").is_empty() {
        blob_of(bytes, "audio/ogg")
    } else if !element
        .can_play_type("audio/webm; codecs=\"opus\"")
        .is_empty()
    {
        blob_of(
            &thiscord_shared::voice_message::ogg_to_webm(&parsed),
            "audio/webm",
        )
    } else {
        Err("This browser can't play voice messages".into())
    }
}

async fn browser_play(ui: Ui, source: Source, position_ms: u32, rate: f32) -> Result<(), String> {
    let key = source.key();
    let duration = source.duration();
    let created = BROWSER.with_borrow_mut(|slot| {
        if slot.is_none() {
            *slot = BrowserPlayer::new();
        }
        if let Some(player) = slot.as_mut() {
            player.release();
        }
        slot.as_ref().map(|p| p.element.clone())
    });
    let element = created.ok_or("Playback unavailable")?;
    let bytes = match &source {
        Source::Recording { blob, .. } => {
            blob_bytes(blob.as_ref().ok_or("Record a voice message first")?).await?
        }
        Source::Attachment {
            attachment,
            private,
        } => {
            let token = ui
                .token
                .get_untracked()
                .ok_or("Sign in again to use voice messages")?;
            let size = attachment.size_bytes.min(MAX_VOICE_MESSAGE_BYTES as u64);
            let _permit = super::chat::AttachmentPermit::acquire(size).await?;
            let path = if *private {
                format!("{DIRECT_ATTACHMENTS_PATH}/{}", attachment.id)
            } else {
                format!("{ATTACHMENTS_PATH}/{}", attachment.id)
            };
            let blob =
                super::chat::attachment_transfer("GET", &path, &token, None, size, |_| {}).await?;
            blob_bytes(&blob).await?
        }
    };
    // The user may have chosen another message while this one downloaded.
    if !now().get_untracked().is_some_and(|n| n.key == key) {
        return Ok(());
    }
    let blob = playable(&element, &bytes)?;
    let url =
        web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| "Playback unavailable")?;
    let settings = ui.audio.get_untracked();
    if let Some(device) = settings.output.as_deref().filter(|d| !d.is_empty())
        && let Ok(function) = js_sys::Reflect::get(&element, &"setSinkId".into())
        && let Ok(function) = function.dyn_into::<js_sys::Function>()
        && let Ok(promise) = function.call1(&element, &device.into())
        && let Ok(promise) = promise.dyn_into::<js_sys::Promise>()
    {
        // A missing device falls back to the default output.
        let _ = JsFuture::from(promise).await;
    }
    element.set_volume(f64::from(settings.output_volume.clamp(0.0, 1.0)));
    element.set_src(&url);
    element.set_playback_rate(f64::from(rate));
    if position_ms > 0 && position_ms + 250 < duration {
        element.set_current_time(f64::from(position_ms) / 1000.0);
    }
    BROWSER.with_borrow_mut(|slot| {
        if let Some(player) = slot.as_mut() {
            player.key = key.clone();
            player.url = Some(url);
            player.duration_ms = duration;
            play_element(&player.element);
            player.track();
        }
    });
    set_state(&key, State::Playing, position_ms, duration, String::new());
    Ok(())
}

// ---- Player ----------------------------------------------------------------

/// Waveform bars with a played overlay; a scrubbable slider.
#[component]
fn Waveform(
    bars: Vec<u8>,
    #[prop(into)] progress: Signal<f64>,
    #[prop(into)] label: Signal<String>,
    #[prop(into)] value_text: Signal<String>,
    #[prop(into)] duration_ms: Signal<u32>,
    #[prop(into)] position_ms: Signal<u32>,
    on_seek: Callback<u32>,
) -> impl IntoView {
    let node = NodeRef::<leptos::html::Div>::new();
    let scrub = RwSignal::new(None::<f64>);
    let fraction_at = move |x: f64| {
        node.get_untracked().map_or(0.0, |n| {
            let rect = n.get_bounding_client_rect();
            ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0)
        })
    };
    let shown = move || scrub.get().unwrap_or_else(|| progress.get());
    let count = bars.len();
    view! {
        <div node_ref=node class="voice-wave" role="slider" tabindex="0"
            aria-label=move || label.get()
            aria-valuemin="0" aria-valuemax=move || (duration_ms.get() / 1000).to_string()
            aria-valuenow=move || (position_ms.get() / 1000).to_string()
            aria-valuetext=move || value_text.get()
            on:pointerdown=move |ev: leptos::ev::PointerEvent| {
                if ev.button() != 0 { return; }
                ev.prevent_default();
                if let Some(n) = node.get_untracked() {
                    let _ = n.set_pointer_capture(ev.pointer_id());
                    let _ = n.focus();
                }
                scrub.set(Some(fraction_at(f64::from(ev.client_x()))));
            }
            on:pointermove=move |ev: leptos::ev::PointerEvent| {
                if scrub.get_untracked().is_some() {
                    scrub.set(Some(fraction_at(f64::from(ev.client_x()))));
                }
            }
            on:pointerup=move |ev: leptos::ev::PointerEvent| {
                if scrub.get_untracked().is_some() {
                    let at = fraction_at(f64::from(ev.client_x()));
                    scrub.set(None);
                    on_seek.run((at * f64::from(duration_ms.get_untracked())) as u32);
                }
            }
            on:pointercancel=move |_| scrub.set(None)
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if let Some(target) = voice_clip::step(position_ms.get_untracked(), duration_ms.get_untracked(), &ev.key()) {
                    ev.prevent_default();
                    on_seek.run(target);
                }
            }>
            {bars.into_iter().enumerate().map(|(i, height)| {
                let played = move || (i as f64 + 0.5) / count as f64 <= shown();
                // Complete class strings for the Tailwind scanner.
                view! {
                    <span class="voice-bar" class:voice-bar-played=played
                        style:height=format!("{}%", 16 + u32::from(height) * 84 / 255)></span>
                }
            }).collect_view()}
        </div>
    }
}

/// Inline player for a voice-message attachment (guild or private).
#[component]
pub(super) fn Player(ui: Ui, attachment: ChatAttachment, private: bool) -> impl IntoView {
    let voice = attachment
        .voice
        .clone()
        .unwrap_or(thiscord_shared::chat::VoiceMetadata {
            duration_ms: 0,
            waveform: String::new(),
        });
    let key = attachment.id.to_string();
    let source = Source::Attachment {
        attachment,
        private,
    };
    view! { <PlayerView ui source clip=key duration_ms=voice.duration_ms waveform=voice.waveform/> }
}

#[component]
fn PlayerView(
    ui: Ui,
    source: Source,
    /// Playback key: the attachment ID, or "recording" for the preview.
    clip: String,
    duration_ms: u32,
    waveform: String,
    #[prop(optional)] compact: bool,
) -> impl IntoView {
    let current = now();
    let mine = Memo::new(move |_| current.get().filter(|n| n.key == clip));
    let state = move || mine.get().map(|n| n.state);
    let position = Signal::derive(move || mine.get().map_or(0, |n| n.position_ms));
    let total = Signal::derive(move || {
        mine.get()
            .map(|n| n.duration_ms)
            .filter(|d| *d > 0)
            .unwrap_or(duration_ms)
    });
    let progress = Signal::derive(move || {
        let total = total.get();
        if total == 0 {
            0.0
        } else {
            f64::from(position.get()) / f64::from(total)
        }
    });
    let active = move || {
        matches!(
            state(),
            Some(State::Playing | State::Paused | State::Loading)
        )
    };
    let playing = move || state() == Some(State::Playing);
    let loading = move || state() == Some(State::Loading);
    let error = move || {
        mine.get()
            .filter(|n| n.state == State::Error)
            .map(|n| n.error)
    };
    let time = move || {
        if active() {
            voice_clip::clock(position.get())
        } else {
            voice_clip::clock(total.get())
        }
    };
    let speed_signal = speed();
    let rate = Signal::derive(move || speed_signal.get());
    // Recordings hold a browser Blob; keep the source in local storage.
    let source = StoredValue::new_local(source);
    let on_seek = Callback::new(move |at: u32| seek(ui, source.get_value(), at));
    let label = Signal::derive(move || crate::i18n::t("Voice message position").to_owned());
    let value_text = Signal::derive(move || {
        thiscord_frontend::localized_format!(
            crate::i18n::language(),
            "{} of {}",
            voice_clip::clock(position.get()),
            voice_clip::clock(total.get())
        )
    });
    let class = if compact {
        "voice-player voice-player-compact"
    } else {
        "voice-player"
    };
    view! {
        <div class=class role="group" aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "Voice message, {}", voice_clip::clock(total.get()))>
            <button type="button" class="voice-play" class:voice-play-active=playing
                aria-label=move || if playing() { crate::i18n::t("Pause voice message") } else { crate::i18n::t("Play voice message") }
                on:click=move |_| toggle(ui, source.get_value())>
                {move || if loading() {
                    view! { <span class="voice-spinner" aria-hidden="true"></span> }.into_any()
                } else if playing() {
                    view! { <Icon kind=Glyph::Pause/> }.into_any()
                } else {
                    view! { <Icon kind=Glyph::Play/> }.into_any()
                }}
            </button>
            <Waveform bars=voice_clip::bars(&waveform, BARS) progress label value_text duration_ms=total position_ms=position on_seek/>
            <span class="voice-time" aria-hidden="true">{time}</span>
            <button type="button" class="voice-speed" aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "Playback speed {}", voice_clip::speed_label(rate.get()))
                data-tip=move || crate::i18n::t("Playback speed") on:click=move |_| change_speed()>
                {move || voice_clip::speed_label(rate.get())}
            </button>
            <Show when=move || error().is_some()>
                <p class="voice-error" role="status">{move || error().map(crate::i18n::message).unwrap_or_default()}</p>
            </Show>
        </div>
    }
}

// ---- Recorder --------------------------------------------------------------

/// Where a finished recording is uploaded.
#[derive(Clone, Copy)]
pub(super) enum Target {
    Guild { guild: GuildId, channel: ChannelId },
    Direct { conversation: ConversationId },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Preview,
    Uploading,
}

/// A browser recording in progress: MediaRecorder plus an analyser for levels.
struct BrowserRecording {
    stream: web_sys::MediaStream,
    recorder: web_sys::MediaRecorder,
    context: Option<web_sys::AudioContext>,
    _analyser: Option<web_sys::AnalyserNode>,
    chunks: Rc<RefCell<Vec<web_sys::Blob>>>,
    levels: Rc<RefCell<Vec<f32>>>,
    started: f64,
    _data: Closure<dyn FnMut(web_sys::BlobEvent)>,
    _stop: Option<Closure<dyn FnMut()>>,
}
impl BrowserRecording {
    fn close(&self) {
        for track in self.stream.get_tracks().iter() {
            if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
                track.stop();
            }
        }
        // Idempotent: closing twice rejects, so close once and await quietly.
        if let Some(context) = &self.context
            && context.state() != web_sys::AudioContextState::Closed
            && let Ok(promise) = context.close()
        {
            leptos::task::spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
    }
}
impl Drop for BrowserRecording {
    fn drop(&mut self) {
        self.recorder.set_ondataavailable(None);
        self.recorder.set_onstop(None);
        if self.recorder.state() != web_sys::RecordingState::Inactive {
            let _ = self.recorder.stop();
        }
        self.close();
    }
}

fn microphone_problem(error: &JsValue) -> &'static str {
    let name = js_sys::Reflect::get(error, &"name".into())
        .ok()
        .and_then(|n| n.as_string())
        .unwrap_or_default();
    match name.as_str() {
        "NotAllowedError" | "SecurityError" | "PermissionDeniedError" => {
            "Microphone access is blocked. Allow the microphone for this site, then try again."
        }
        "NotFoundError" | "OverconstrainedError" | "DevicesNotFoundError" => "No microphone found",
        "NotReadableError" | "AbortError" | "TrackStartError" => {
            "The microphone is busy or unavailable"
        }
        _ => "Cannot open the microphone",
    }
}

fn recorder_mime() -> Option<&'static str> {
    if !js_sys::Reflect::has(&js_sys::global(), &"MediaRecorder".into()).unwrap_or(false) {
        return None;
    }
    ["audio/ogg;codecs=opus", "audio/webm;codecs=opus"]
        .into_iter()
        .find(|mime| web_sys::MediaRecorder::is_type_supported(mime))
}

async fn open_browser_microphone(
    device: Option<String>,
) -> Result<web_sys::MediaStream, &'static str> {
    let window = web_sys::window().ok_or("Cannot open the microphone")?;
    if !window.is_secure_context() {
        return Err("Voice messages need a secure (HTTPS) page to use your microphone");
    }
    let devices = window
        .navigator()
        .media_devices()
        .map_err(|_| "Cannot open the microphone")?;
    let request = |device: Option<&str>| {
        let mut audio = serde_json::json!({"echoCancellation":true,"noiseSuppression":true,"autoGainControl":true,"channelCount":1});
        if let Some(device) = device {
            audio["deviceId"] = serde_json::json!({"exact":device});
        }
        let constraints = web_sys::MediaStreamConstraints::new();
        constraints.set_audio(&js_sys::JSON::parse(&audio.to_string()).unwrap_or(JsValue::TRUE));
        constraints.set_video(&JsValue::FALSE);
        devices.get_user_media_with_constraints(&constraints)
    };
    let device = device.filter(|d| !d.is_empty());
    let mut result = match request(device.as_deref()) {
        Ok(promise) => JsFuture::from(promise).await,
        Err(error) => Err(error),
    };
    if device.is_some()
        && let Err(error) = &result
        && microphone_problem(error) == "No microphone found"
    {
        result = match request(None) {
            Ok(promise) => JsFuture::from(promise).await,
            Err(error) => Err(error),
        };
    }
    result
        .map(|stream| stream.unchecked_into())
        .map_err(|error| microphone_problem(&error))
}

/// Upload result for the owner to send as its own message.
#[derive(Clone)]
pub(super) struct Recorded {
    pub attachment: ChatAttachment,
    pub spoiler: bool,
}

/// The composer's microphone button and the recording/preview bar that covers
/// the composer while active. Click toggles a hands-free recording; pressing
/// and holding records until release (send), slide left to cancel, slide up
/// to lock. Keyboard: Enter/Space start, Escape cancels, Enter sends.
#[component]
pub(super) fn Recorder(
    ui: Ui,
    target: Target,
    #[prop(into)] disabled: Signal<bool>,
    /// Sends the uploaded voice message; returns once queued.
    on_send: Callback<Recorded>,
) -> impl IntoView {
    let phase = RwSignal::new(Phase::Idle);
    let elapsed = RwSignal::new(0u32);
    let live = RwSignal::new(VecDeque::from(vec![0u8; LIVE_BARS]));
    let waveform = RwSignal::new(String::new());
    let duration = RwSignal::new(0u32);
    let spoiler = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let locked = RwSignal::new(true);
    let gesture = RwSignal::new(Gesture::Holding);
    let press = StoredValue::new(None::<(f64, f64, f64)>);
    // Release during a hold sends as soon as the recording is ready.
    let send_when_ready = StoredValue::new(false);
    let browser = StoredValue::new_local(None::<BrowserRecording>);
    let browser_blob = StoredValue::new_local(None::<web_sys::Blob>);
    let browser_timer = StoredValue::new_local(None::<gloo_timers::callback::Interval>);
    let listener = StoredValue::new_local(None::<client::EventListener>);
    // Native recording session this recorder started (events carry it).
    let native_session = StoredValue::new(None::<u64>);
    let stop_button = NodeRef::<leptos::html::Button>::new();
    let mic_button = NodeRef::<leptos::html::Button>::new();
    let send_button = NodeRef::<leptos::html::Button>::new();
    let alive = RwSignal::new(true);

    let push_level = move |level: f32| {
        live.update(|bars| {
            bars.pop_front();
            bars.push_back(voice_clip::level_bar(level));
        });
    };
    let reset = move || {
        phase.set(Phase::Idle);
        elapsed.set(0);
        live.set(VecDeque::from(vec![0u8; LIVE_BARS]));
        waveform.set(String::new());
        duration.set(0);
        spoiler.set(false);
        gesture.set(Gesture::Holding);
        send_when_ready.set_value(false);
        browser.set_value(None);
        browser_blob.set_value(None);
        browser_timer.set_value(None);
    };
    let focus_mic = move || {
        request_animation_frame(move || {
            if let Some(button) = mic_button.get_untracked() {
                let _ = button.focus();
            }
        });
    };

    let upload = move || {
        if phase.get_untracked() != Phase::Preview {
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        phase.set(Phase::Uploading);
        error.set(String::new());
        if now().get_untracked().is_some_and(|n| n.key == "recording") {
            stop_all();
        }
        let wave = waveform.get_untracked();
        let blob = browser_blob.get_value();
        leptos::task::spawn_local(async move {
            let result: Result<ChatAttachment, String> = if client::desktop() {
                let target = match target {
                    Target::Guild { guild, channel } => {
                        serde_json::json!({"kind":"guild","guild_id":guild,"channel_id":channel})
                    }
                    Target::Direct { conversation } => {
                        serde_json::json!({"kind":"direct","conversation_id":conversation})
                    }
                };
                client::native(
                    "voice_message_upload",
                    serde_json::json!({"token":token,"target":target}),
                )
                .await
            } else {
                async {
                    let blob = blob.ok_or("Record a voice message first")?;
                    let common = format!(
                        "filename=voice-message.ogg&content_type=audio%2Fogg&voice=true&waveform={wave}"
                    );
                    let path = match target {
                        Target::Guild { guild, channel } => format!("{ATTACHMENTS_PATH}?guild_id={guild}&channel_id={channel}&{common}"),
                        Target::Direct { conversation } => format!("{DIRECT_ATTACHMENTS_PATH}?conversation_id={conversation}&{common}"),
                    };
                    let response = super::chat::attachment_transfer("POST", &path, &token, Some(&blob), 32768, |_| {}).await?;
                    let text = JsFuture::from(response.text())
                        .await
                        .ok()
                        .and_then(|t| t.as_string())
                        .ok_or("Invalid upload response")?;
                    serde_json::from_str::<ChatAttachment>(&text).map_err(|_| "Invalid upload response".to_owned())
                }
                .await
            };
            if alive.try_get_untracked() != Some(true)
                || ui.token.get_untracked().as_deref() != Some(token.as_str())
            {
                return;
            }
            match result {
                Ok(attachment) => {
                    let recorded = Recorded {
                        attachment,
                        spoiler: spoiler.get_untracked(),
                    };
                    reset();
                    on_send.run(recorded);
                    focus_mic();
                }
                Err(message) => {
                    phase.set(Phase::Preview);
                    error.set(message);
                }
            }
        });
    };

    let ready = move |length: u32, wave: String, limit: bool| {
        duration.set(length);
        waveform.set(wave);
        phase.set(Phase::Preview);
        if limit {
            notice.set(crate::i18n::t("Reached the 5-minute limit").into());
        }
        if send_when_ready.get_value() {
            send_when_ready.set_value(false);
            upload();
        } else {
            request_animation_frame(move || {
                if let Some(button) = send_button.get_untracked() {
                    let _ = button.focus();
                }
            });
        }
    };

    #[derive(serde::Deserialize)]
    struct NativeRecorder {
        session: u64,
        state: String,
        elapsed_ms: u32,
        level: f32,
        #[serde(default)]
        waveform: Option<String>,
        #[serde(default)]
        error: Option<String>,
    }
    if client::desktop() {
        leptos::task::spawn_local(async move {
            let result = client::listen("voice-message-recorder", move |event: NativeRecorder| {
                if alive.try_get_untracked() != Some(true)
                    || native_session.try_get_value() != Some(Some(event.session))
                {
                    return;
                }
                let current = phase.get_untracked();
                if !matches!(
                    current,
                    Phase::Recording | Phase::Stopping | Phase::Starting
                ) {
                    return;
                }
                match event.state.as_str() {
                    "recording" => {
                        elapsed.set(event.elapsed_ms);
                        push_level(event.level);
                    }
                    "ready" | "limit" => ready(
                        event.elapsed_ms,
                        event
                            .waveform
                            .unwrap_or_else(|| encode_waveform(&[0; WAVEFORM_BARS])),
                        event.state == "limit",
                    ),
                    "too_short" => {
                        reset();
                        notice.set(crate::i18n::t("Hold to record, release to send").into());
                        focus_mic();
                    }
                    "cancelled" => reset(),
                    _ => {
                        reset();
                        error.set(native_microphone_error(event.error.unwrap_or_default()));
                    }
                }
            })
            .await;
            if alive.try_get_untracked() == Some(true) {
                listener.set_value(result.ok());
            }
        });
    }

    let finish_browser = move |send: bool| {
        // MediaRecorder delivers its final chunk before `stop`.
        let Some(recording) = browser.with_value(|r| {
            r.as_ref().map(|r| {
                (
                    r.recorder.clone(),
                    r.chunks.clone(),
                    r.levels.clone(),
                    r.started,
                )
            })
        }) else {
            return;
        };
        let (recorder, chunks, levels, started) = recording;
        send_when_ready.set_value(send);
        let on_stop = Closure::<dyn FnMut()>::new(move || {
            let chunks = chunks.borrow().clone();
            let levels = levels.borrow().clone();
            let elapsed_ms = (js_sys::Date::now() - started).max(0.0) as u32;
            leptos::task::spawn_local(async move {
                if alive.try_get_untracked() != Some(true)
                    || phase.get_untracked() != Phase::Stopping
                {
                    return;
                }
                let result = async {
                    let parts = js_sys::Array::new();
                    for chunk in &chunks {
                        parts.push(chunk);
                    }
                    let whole = web_sys::Blob::new_with_blob_sequence(&parts)
                        .map_err(|_| "Cannot read the recording")?;
                    if whole.size() > thiscord_shared::voice_message::MAX_RECORDER_BYTES as f64 {
                        return Err("Recording is too large".to_owned());
                    }
                    let bytes = blob_bytes(&whole).await?;
                    let serial = (js_sys::Math::random() * f64::from(u32::MAX)) as u32 | 1;
                    let ogg = if bytes.starts_with(b"OggS") {
                        let parsed = thiscord_shared::voice_message::parse_ogg_opus(&bytes)
                            .map_err(|e| e.0.to_owned())?;
                        thiscord_shared::voice_message::rewrite_ogg(&parsed, serial)
                    } else {
                        thiscord_shared::voice_message::webm_to_ogg(&bytes, serial)
                            .map_err(|e| e.0.to_owned())?
                    };
                    let parsed = thiscord_shared::voice_message::parse_ogg_opus(&ogg)
                        .map_err(|e| e.0.to_owned())?;
                    // The container is authoritative; the wall clock only bounds it.
                    Ok::<_, String>((
                        ogg,
                        parsed.duration_ms.min(elapsed_ms.saturating_add(1_000)),
                    ))
                }
                .await;
                if alive.try_get_untracked() != Some(true)
                    || phase.get_untracked() != Phase::Stopping
                {
                    return;
                }
                browser.set_value(None);
                match result {
                    Ok((_, length)) if length < MIN_VOICE_MESSAGE_MS => {
                        reset();
                        notice.set(crate::i18n::t("Hold to record, release to send").into());
                        focus_mic();
                    }
                    Ok((ogg, length)) => match blob_of(&ogg, "audio/ogg") {
                        Ok(blob) => {
                            browser_blob.set_value(Some(blob));
                            let limit = length + 500 >= MAX_VOICE_MESSAGE_MS;
                            ready(
                                length.min(MAX_VOICE_MESSAGE_MS),
                                encode_waveform(&thiscord_shared::voice_message::waveform(&levels)),
                                limit,
                            );
                        }
                        Err(message) => {
                            reset();
                            error.set(message);
                        }
                    },
                    Err(message) => {
                        reset();
                        error.set(message);
                    }
                }
            });
        });
        recorder.set_onstop(Some(on_stop.as_ref().unchecked_ref()));
        browser.update_value(|r| {
            if let Some(r) = r.as_mut() {
                r._stop = Some(on_stop);
            }
        });
        browser_timer.set_value(None);
        let _ = recorder.stop();
        browser.with_value(|r| {
            if let Some(r) = r {
                r.close();
            }
        });
    };

    let stop = move |send: bool| {
        if phase.get_untracked() != Phase::Recording {
            return;
        }
        phase.set(Phase::Stopping);
        if client::desktop() {
            send_when_ready.set_value(send);
            leptos::task::spawn_local(async move {
                if let Err(message) =
                    client::native::<()>("voice_message_record_stop", serde_json::json!({})).await
                {
                    reset();
                    error.set(message);
                }
            });
        } else {
            finish_browser(send);
        }
    };

    let cancel = move || {
        let current = phase.get_untracked();
        if current == Phase::Idle || current == Phase::Uploading {
            return;
        }
        if now().get_untracked().is_some_and(|n| n.key == "recording") {
            stop_all();
        }
        if client::desktop() {
            leptos::task::spawn_local(async {
                let _ = client::native::<()>("voice_message_record_cancel", serde_json::json!({}))
                    .await;
            });
        }
        reset();
        notice.set(String::new());
        focus_mic();
    };

    let begin = move |hands_free: bool| {
        if disabled.get_untracked() || phase.get_untracked() != Phase::Idle {
            return;
        }
        error.set(String::new());
        notice.set(String::new());
        locked.set(hands_free);
        stop_all();
        phase.set(Phase::Starting);
        if client::desktop() {
            leptos::task::spawn_local(async move {
                let result =
                    client::native::<u64>("voice_message_record_start", serde_json::json!({}))
                        .await;
                if alive.try_get_untracked() != Some(true) {
                    return;
                }
                match result {
                    Ok(session) => {
                        native_session.set_value(Some(session));
                        if phase.get_untracked() == Phase::Starting {
                            phase.set(Phase::Recording);
                            request_animation_frame(move || {
                                if let Some(button) = stop_button.get_untracked() {
                                    let _ = button.focus();
                                }
                            });
                        } else {
                            let _ = client::native::<()>(
                                "voice_message_record_cancel",
                                serde_json::json!({}),
                            )
                            .await;
                        }
                    }
                    Err(message) => {
                        reset();
                        error.set(native_microphone_error(message));
                    }
                }
            });
            return;
        }
        let Some(mime) = recorder_mime() else {
            reset();
            error.set(crate::i18n::t("Voice messages can't be recorded in this browser").into());
            return;
        };
        let device = ui.audio.get_untracked().input;
        leptos::task::spawn_local(async move {
            let stream = match open_browser_microphone(device).await {
                Ok(stream) => stream,
                Err(message) => {
                    if alive.try_get_untracked() == Some(true) {
                        reset();
                        error.set(crate::i18n::t(message).into());
                    }
                    return;
                }
            };
            if alive.try_get_untracked() != Some(true) || phase.get_untracked() != Phase::Starting {
                for track in stream.get_tracks().iter() {
                    if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
                        track.stop();
                    }
                }
                return;
            }
            let options = web_sys::MediaRecorderOptions::new();
            options.set_mime_type(mime);
            options.set_audio_bits_per_second(32_000);
            let Ok(recorder) =
                web_sys::MediaRecorder::new_with_media_stream_and_media_recorder_options(
                    &stream, &options,
                )
            else {
                reset();
                error
                    .set(crate::i18n::t("Voice messages can't be recorded in this browser").into());
                return;
            };
            let chunks = Rc::new(RefCell::new(Vec::new()));
            let collected = chunks.clone();
            let data =
                Closure::<dyn FnMut(web_sys::BlobEvent)>::new(move |event: web_sys::BlobEvent| {
                    if let Some(blob) = event.data()
                        && blob.size() > 0.0
                    {
                        collected.borrow_mut().push(blob);
                    }
                });
            recorder.set_ondataavailable(Some(data.as_ref().unchecked_ref()));
            // Levels come from an analyser on the same capture, off the recorder.
            let context = web_sys::AudioContext::new().ok();
            let analyser = context.as_ref().and_then(|context| {
                let source = context.create_media_stream_source(&stream).ok()?;
                let analyser = context.create_analyser().ok()?;
                analyser.set_fft_size(1024);
                source.connect_with_audio_node(&analyser).ok()?;
                Some(analyser)
            });
            if recorder.start_with_time_slice(1000).is_err() {
                reset();
                error.set(crate::i18n::t("Cannot open the microphone").into());
                return;
            }
            let levels = Rc::new(RefCell::new(Vec::new()));
            let started = js_sys::Date::now();
            let sample_analyser = analyser.clone();
            let sampled = levels.clone();
            let mut buffer = vec![0f32; 1024];
            browser_timer.set_value(Some(gloo_timers::callback::Interval::new(50, move || {
                if alive.try_get_untracked() != Some(true)
                    || phase.get_untracked() != Phase::Recording
                {
                    return;
                }
                let peak = sample_analyser.as_ref().map_or(0.0, |analyser| {
                    analyser.get_float_time_domain_data(&mut buffer);
                    buffer.iter().fold(0f32, |a, b| a.max(b.abs()))
                });
                // Two 20 ms-equivalent entries per tick keep the waveform's time scale.
                sampled.borrow_mut().push(peak * 0.7);
                let ms = (js_sys::Date::now() - started).max(0.0) as u32;
                elapsed.set(ms.min(MAX_VOICE_MESSAGE_MS));
                push_level(peak);
                if ms >= MAX_VOICE_MESSAGE_MS {
                    // Stopping drops this interval; never do that inside its own callback.
                    leptos::task::spawn_local(async move { stop(false) });
                }
            })));
            browser.set_value(Some(BrowserRecording {
                stream,
                recorder,
                context,
                _analyser: analyser,
                chunks,
                levels,
                started,
                _data: data,
                _stop: None,
            }));
            phase.set(Phase::Recording);
            request_animation_frame(move || {
                if let Some(button) = stop_button.get_untracked() {
                    let _ = button.focus();
                }
            });
        });
    };

    on_cleanup(move || {
        alive.set(false);
        listener.set_value(None);
        if matches!(
            phase.get_untracked(),
            Phase::Starting | Phase::Recording | Phase::Stopping | Phase::Preview
        ) && client::desktop()
        {
            leptos::task::spawn_local(async {
                let _ = client::native::<()>("voice_message_record_cancel", serde_json::json!({}))
                    .await;
            });
        }
        if now().get_untracked().is_some_and(|n| n.key == "recording") {
            stop_all();
        }
        browser_timer.set_value(None);
        browser.set_value(None);
    });
    // Losing the session or permission discards the recording.
    Effect::new(move |previous: Option<Option<String>>| {
        let token = ui.token.get();
        if previous.is_some_and(|p| p != token) {
            cancel();
        }
        token
    });
    Effect::new(move |_| {
        if disabled.get() && phase.get_untracked() != Phase::Uploading {
            cancel();
        }
    });

    // Press-and-hold on the microphone button.
    let pointer_down = move |ev: leptos::ev::PointerEvent| {
        if ev.button() != 0 || disabled.get_untracked() || phase.get_untracked() != Phase::Idle {
            return;
        }
        ev.prevent_default();
        if let Some(button) = mic_button.get_untracked() {
            let _ = button.set_pointer_capture(ev.pointer_id());
        }
        press.set_value(Some((
            f64::from(ev.client_x()),
            f64::from(ev.client_y()),
            js_sys::Date::now(),
        )));
        gesture.set(Gesture::Holding);
        begin(false);
    };
    let pointer_move = move |ev: leptos::ev::PointerEvent| {
        if let Some((x, y, _)) = press.get_value()
            && !locked.get_untracked()
        {
            let next =
                voice_clip::gesture(f64::from(ev.client_x()) - x, f64::from(ev.client_y()) - y);
            gesture.set(next);
            if next == Gesture::Lock {
                locked.set(true);
                press.set_value(None);
            }
        }
    };
    let pointer_up = move |_| {
        let Some((_, _, at)) = press.get_value() else {
            return;
        };
        press.set_value(None);
        if js_sys::Date::now() - at < voice_clip::TAP_MILLISECONDS {
            // A tap starts a hands-free recording.
            locked.set(true);
            return;
        }
        match gesture.get_untracked() {
            Gesture::Cancel => cancel(),
            Gesture::Lock => {}
            Gesture::Holding => {
                if phase.get_untracked() == Phase::Starting {
                    // Released before the microphone opened: treat as a tap.
                    locked.set(true);
                } else {
                    stop(true);
                }
            }
        }
    };
    let pointer_cancel = move |_| {
        if press.get_value().is_some() {
            press.set_value(None);
            cancel();
        }
    };
    let mic_keydown = move |ev: leptos::ev::KeyboardEvent| {
        if (ev.key() == "Enter" || ev.key() == " ") && !ev.repeat() {
            ev.prevent_default();
            begin(true);
        }
    };
    let bar_keydown = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Escape" {
            ev.prevent_default();
            ev.stop_propagation();
            cancel();
        }
    };
    let preview_source = move || Source::Recording {
        blob: browser_blob.get_value(),
        duration_ms: duration.get_untracked(),
    };
    let remaining_warning = move || elapsed.get() + 30_000 >= MAX_VOICE_MESSAGE_MS;
    let active = move || phase.get() != Phase::Idle;
    let live_dot = move || phase.get() == Phase::Recording;
    let not_recording = move || phase.get() != Phase::Recording;
    let uploading = move || phase.get() == Phase::Uploading;
    let show_hint = move || !locked.get() && phase.get() == Phase::Recording;
    let show_controls = move || locked.get() || phase.get() != Phase::Recording;
    let cancelling = move || gesture.get() == Gesture::Cancel;
    let live_scale = move |i: usize| {
        let height = live.with(|bars| bars.get(i).copied().unwrap_or(0));
        format!("scaleY({:.3})", 0.12 + f32::from(height) / 255.0 * 0.88)
    };
    let timer_text = move || {
        if phase.get() == Phase::Starting {
            crate::i18n::t("Starting…").to_owned()
        } else {
            voice_clip::clock(elapsed.get())
        }
    };
    let hint_text = move || {
        if gesture.get() == Gesture::Cancel {
            crate::i18n::t("Release to cancel")
        } else {
            crate::i18n::t("◀ Slide to cancel · ▲ lock")
        }
    };
    let send_icon = move || {
        if phase.get() == Phase::Uploading {
            view! { <span class="voice-spinner" aria-hidden="true"></span> }.into_any()
        } else {
            view! { <Icon kind=Glyph::Send/> }.into_any()
        }
    };
    view! {
        <button node_ref=mic_button type="button" class="icon-button voice-mic" class:voice-mic-hot=move || matches!(phase.get(), Phase::Starting | Phase::Recording)
            aria-label=move || crate::i18n::t("Record voice message")
            data-tip=move || format!("{}\n{}", crate::i18n::t("Record voice message"), crate::i18n::t("Click to record · hold to talk, release to send"))
            disabled=move || disabled.get() || matches!(phase.get(), Phase::Preview | Phase::Uploading | Phase::Stopping)
            on:pointerdown=pointer_down on:pointermove=pointer_move on:pointerup=pointer_up on:pointercancel=pointer_cancel
            on:keydown=mic_keydown on:click=move |ev| ev.prevent_default()>
            <Icon kind=Glyph::Mic/>
        </button>
        <Show when=active>
            <div class="voice-recorder" role="group" aria-label=move || crate::i18n::t("Voice message recorder") on:keydown=bar_keydown>
                <button type="button" class="icon-button voice-discard" aria-label=move || crate::i18n::t("Discard voice message")
                    data-tip=move || crate::i18n::t("Discard (Esc)") disabled=move || phase.get() == Phase::Uploading on:click=move |_| cancel()>
                    <Icon kind=Glyph::Trash/>
                </button>
                {move || match phase.get() {
                    Phase::Starting | Phase::Recording | Phase::Stopping => view! {
                        <span class="voice-rec-dot" class:voice-rec-dot-live=live_dot aria-hidden="true"></span>
                        <span class="voice-rec-time" class:text-warning=remaining_warning role="timer" aria-live="off">{timer_text}</span>
                        <div class="voice-live" aria-hidden="true">
                            {(0..LIVE_BARS).map(|i| view! { <span class="voice-live-bar" style:transform=move || live_scale(i)></span> }).collect_view()}
                        </div>
                        <Show when=show_hint>
                            <span class="voice-hint" class:voice-hint-cancel=cancelling>{hint_text}</span>
                        </Show>
                        <Show when=show_controls>
                            <button node_ref=stop_button type="button" class="icon-button voice-stop" aria-label=move || crate::i18n::t("Stop and review")
                                data-tip=move || crate::i18n::t("Stop and review") disabled=not_recording on:click=move |_| stop(false)>
                                <Icon kind=Glyph::Stop/>
                            </button>
                            <button type="button" class="composer-send" aria-label=move || crate::i18n::t("Send voice message")
                                data-tip=move || crate::i18n::t("Send") disabled=not_recording on:click=move |_| stop(true)>
                                <Icon kind=Glyph::Send/>
                            </button>
                        </Show>
                    }.into_any(),
                    _ => view! {
                        <PlayerView ui source=preview_source() clip="recording".to_owned() duration_ms=duration.get_untracked() waveform=waveform.get_untracked() compact=true/>
                        <button type="button" class="icon-button voice-spoiler" class:voice-spoiler-on=move || spoiler.get() aria-pressed=move || spoiler.get().to_string()
                            aria-label=move || crate::i18n::t("Mark as spoiler") data-tip=move || crate::i18n::t("Mark as spoiler")
                            on:click=move |_| spoiler.update(|s| *s = !*s)>
                            <Icon kind=Glyph::EyeOff/>
                        </button>
                        <button node_ref=send_button type="button" class="composer-send" aria-label=move || crate::i18n::t("Send voice message")
                            data-tip=move || crate::i18n::t("Send") disabled=uploading on:click=move |_| upload()>
                            {send_icon}
                        </button>
                    }.into_any(),
                }}
            </div>
        </Show>
        <Show when=move || !error.get().is_empty() || !notice.get().is_empty()>
            <p class="voice-notice" class:voice-notice-error=move || !error.get().is_empty() role="status">
                {move || if error.get().is_empty() { crate::i18n::message(notice.get()) } else { crate::i18n::message(error.get()) }}
                <button type="button" class="voice-notice-close" aria-label=move || crate::i18n::t("Dismiss") on:click=move |_| { error.set(String::new()); notice.set(String::new()); }>
                    <Icon kind=Glyph::Close/>
                </button>
            </p>
        </Show>
    }
}

/// Native capture errors carry device text; show a clear, translated reason.
fn native_microphone_error(message: String) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission") || lower.contains("denied") {
        crate::i18n::t("Microphone access is blocked. Allow Thiscord to use the microphone in your system settings.").into()
    } else if lower.contains("no system default microphone") || lower.contains("disconnected") {
        crate::i18n::t("No microphone found").into()
    } else if message.is_empty() {
        crate::i18n::t("Cannot open the microphone").into()
    } else {
        message
    }
}

/// True for attachments the player can show (validated voice metadata).
pub(super) fn is_voice(attachment: &ChatAttachment) -> bool {
    attachment.voice.is_some() && attachment.content_type == "audio/ogg"
}
