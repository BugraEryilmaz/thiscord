//! Web-client screen sharing, camera controls and players. Capture, encoding
//! and decoding stay in the browser (`browser_voice::video`); the call grid,
//! fullscreen, hide and LIVE badge behavior remain in `screen.rs`.
use super::Ui;
use super::icons::{Glyph, Icon};
use crate::browser_voice::video;
use leptos::prelude::*;
use thiscord_shared::{
    screen::{Quality, Watch},
    voice::MediaKind,
};
use wasm_bindgen::JsCast;
use web_sys::{HtmlMediaElement, HtmlVideoElement, MediaStream, MediaStreamTrack};

/// Phones and tablets, which can flip between front and back cameras.
fn coarse_pointer() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::get(&window, &"matchMedia".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call1(&window, &"(pointer: coarse)".into()).ok())
        .and_then(|query| js_sys::Reflect::get(&query, &"matches".into()).ok())
        .and_then(|matches| matches.as_bool())
        .unwrap_or(false)
}

#[component]
pub(super) fn BrowserScreenControls(ui: Ui) -> impl IntoView {
    let supported = video::screen_supported();
    let audio_supported = video::screen_audio_supported();
    let picking = RwSignal::new(false);
    let quality = RwSignal::new(Quality::default());
    let audio = RwSignal::new(audio_supported);
    let busy = RwSignal::new(false);
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    Effect::new(move |_| {
        if picking.get()
            && let Some(d) = dialog.get()
        {
            super::dialog::open(&d);
        }
    });
    let choose = move |_| {
        video::clear_error();
        if supported {
            picking.set(true);
        } else {
            video::show_error("Screen sharing isn't supported in this browser");
        }
    };
    // getDisplayMedia must start inside this click, before any await.
    let go_live = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let started = video::start_screen(
            quality.get_untracked(),
            audio_supported && audio.get_untracked(),
        );
        leptos::task::spawn_local(async move {
            started.await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            picking.set(false);
        });
    };
    view! {
        <div class="screen-control" aria-label=move || crate::i18n::t("Screen sharing")>
            <Show when=move ||video::state().screen fallback=move ||view!{
                <button class="icon-button voice-action" aria-label=move || crate::i18n::t("Share screen")
                    data-tip=move ||if !supported {crate::i18n::t("Screen sharing isn't supported in this browser")} else if !super::audio::media_publish_allowed(ui) {crate::i18n::t("Undeafen to share")} else {crate::i18n::t("Share screen")}
                    disabled=move ||supported && (!super::audio::media_publish_allowed(ui) || busy.get()) on:click=choose><Icon kind=Glyph::Screen/></button>
            }>
                <button class="icon-button voice-action share-stop" aria-label=move || crate::i18n::t("Stop sharing") data-tip=move || crate::i18n::t("Stop sharing") on:click=move |_|video::stop_screen()><Icon kind=Glyph::Screen/><span class="share-stop-dot"></span></button>
            </Show>
            <Show when=move ||picking.get()>
                <dialog node_ref=dialog class="share-picker" aria-labelledby="browser-share-title" on:cancel=move |e:web_sys::Event|{if busy.get_untracked(){e.prevent_default();}} on:click=move |e|super::dialog::dismiss_outside(e,dialog,busy.get_untracked()) on:close=move |_|picking.set(false)>
                    <header class="share-picker-header"><div><span class="settings-kicker">{move || crate::i18n::t("GO LIVE")}</span><h2 id="browser-share-title">{move || crate::i18n::t("Share your screen")}</h2><p>{move || crate::i18n::t("Your browser asks which screen, window or tab to share.")}</p></div><button class="icon-button" aria-label=move || crate::i18n::t("Close screen picker") disabled=move ||busy.get() on:click=move |_|picking.set(false)><Icon kind=Glyph::Close/></button></header>
                    <div class="share-picker-body">
                        <div class="share-quality"><label>{move || crate::i18n::t("Resolution")}<select aria-label=move || crate::i18n::t("Share resolution") prop:value=move ||quality.get().height.to_string() on:change=move |e|if let Ok(height)=event_target_value(&e).parse(){quality.update(|q|q.height=height);}>
                            <option value="720">"720p"</option><option value="1080">{move || crate::i18n::t("1080p · Recommended")}</option><option value="1440">"1440p"</option><option value="2160">"4K"</option></select></label>
                            <label>{move || crate::i18n::t("Frame rate")}<select aria-label=move || crate::i18n::t("Share frame rate") prop:value=move ||quality.get().fps.to_string() on:change=move |e|if let Ok(fps)=event_target_value(&e).parse(){quality.update(|q|q.fps=fps);}>
                            <option value="15">{move || crate::i18n::t("15 fps · Presentations")}</option><option value="30">{move || crate::i18n::t("30 fps · Balanced")}</option><option value="60">{move || crate::i18n::t("60 fps · Smooth")}</option></select></label>
                        </div>
                        {if audio_supported {
                            view!{<label class="share-audio-toggle"><span><strong>{move || crate::i18n::t("Share audio")}</strong><small>{move || crate::i18n::t("Includes a tab’s audio, or the whole system’s audio on Windows and ChromeOS.")}</small></span><input type="checkbox" prop:checked=move ||audio.get() on:change=move |e|audio.set(event_target_checked(&e))/></label>}.into_any()
                        } else {
                            view!{<p class="share-picker-note">{move || crate::i18n::t("This browser shares video only. Use Chrome or Edge to include audio.")}</p>}.into_any()
                        }}
                        <p class="share-picker-note">{move || crate::i18n::t("Quality adapts to the connection. Microphone mute doesn’t mute shared audio.")}</p>
                    </div>
                    <footer class="share-picker-footer"><p>{move || crate::i18n::t("Your selected source is visible to everyone watching in this call.")}</p><button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|picking.set(false)>{move || crate::i18n::t("Cancel")}</button><button class="settings-primary" disabled=move ||busy.get()||!super::audio::media_publish_allowed(ui) on:click=go_live>{move ||if busy.get(){crate::i18n::t("Please wait…")}else{crate::i18n::t("Go live")}}</button></footer>
                </dialog>
            </Show>
        </div>
    }
}

#[component]
pub(super) fn BrowserCameraControls(ui: Ui) -> impl IntoView {
    let busy = RwSignal::new(false);
    let active = move || video::state().camera;
    // Follow camera selections (voice-bar menu or settings) while publishing;
    // `switch_camera` is serialized, so re-check the selection afterwards.
    let following = StoredValue::new(false);
    let follow = move |from: u32| {
        if following.get_value() {
            return;
        }
        following.set_value(true);
        leptos::task::spawn_local(async move {
            let mut applied = from;
            loop {
                let wanted = crate::cameras::revision_untracked();
                if wanted == applied || !video::state_untracked().camera {
                    break;
                }
                applied = wanted;
                video::switch_camera(false).await;
            }
            following.try_set_value(false);
        });
    };
    let applied = StoredValue::new(crate::cameras::revision_untracked());
    Effect::new(move |_| {
        let revision = crate::cameras::revision();
        let from = applied.get_value();
        applied.set_value(revision);
        if from != revision {
            follow(from);
        }
    });
    let toggle = move |_| {
        if video::state_untracked().camera {
            video::stop_camera();
            return;
        }
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let revision = crate::cameras::revision_untracked();
        leptos::task::spawn_local(async move {
            video::start_camera().await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            // The selection may have changed during the permission prompt.
            follow(revision);
        });
    };
    let flip = coarse_pointer().then(|| {
        Callback::new(move |()| {
            busy.set(true);
            leptos::task::spawn_local(async move {
                video::switch_camera(true).await;
                busy.try_set(false);
            });
        })
    });
    view! {
        <button class="icon-button voice-action" class:active=active disabled=move ||busy.get() || (!active() && !super::audio::media_publish_allowed(ui))
            aria-label=move ||crate::i18n::t("Camera")
            data-tip=move ||if active(){crate::i18n::t("Turn camera off")}else if !super::audio::media_publish_allowed(ui){crate::i18n::t("Undeafen to share")}else{crate::i18n::t("Turn camera on")} aria-pressed=move ||active().to_string() on:click=toggle>
            {move ||view!{<Icon kind=if active(){Glyph::Camera}else{Glyph::CameraOff}/>}}
        </button>
        <super::camera_select::CameraMenu ui=ui flip=flip/>
    }
}

/// Screen-share and camera errors from the browser, shown once for both.
#[component]
pub(super) fn BrowserMediaError() -> impl IntoView {
    view! {
        <Show when=move ||!video::state().error.is_empty()>
            <p class="browser-media-error mt-2 text-xs text-red-300" role="alert">{move ||crate::i18n::message(video::state().error)}</p>
        </Show>
    }
}

/// One grid tile's player: a received track, or this member's own capture.
#[component]
pub(super) fn BrowserVideo(watch: Watch, own: bool) -> impl IntoView {
    let video_node = NodeRef::<leptos::html::Video>::new();
    let message = RwSignal::new("Connecting video...".to_owned());
    let view_id = StoredValue::new(None::<u32>);
    // Tiles scrolled out of a scrolling grid or strip ask for the minimum
    // budget; the observer and its callback live as long as the tile.
    type Observer = (
        web_sys::IntersectionObserver,
        wasm_bindgen::closure::Closure<dyn FnMut(js_sys::Array)>,
    );
    let observer = StoredValue::new_local(None::<Observer>);
    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
    let registration = std::cell::RefCell::new(Some(registration));
    on_cleanup(move || {
        abort.abort();
        if let Some((observer, _)) = observer.try_update_value(Option::take).flatten() {
            observer.disconnect();
        }
        if let Some(id) = view_id.get_value() {
            video::unwatch(id);
        }
    });
    Effect::new(move |_| {
        let Some(element) = video_node.get() else {
            return;
        };
        let Some(registration) = registration.borrow_mut().take() else {
            return;
        };
        let element: HtmlVideoElement = element.clone();
        if !own {
            let id = video::watch(element.clone(), watch);
            view_id.set_value(Some(id));
            let changed = wasm_bindgen::closure::Closure::<dyn FnMut(js_sys::Array)>::new(
                move |entries: js_sys::Array| {
                    if let Some(entry) = entries
                        .iter()
                        .filter_map(|e| e.dyn_into::<web_sys::IntersectionObserverEntry>().ok())
                        .next_back()
                    {
                        video::set_off_screen(id, !entry.is_intersecting());
                    }
                },
            );
            if let Ok(created) =
                web_sys::IntersectionObserver::new(changed.as_ref().unchecked_ref())
            {
                created.observe(&element);
                observer.set_value(Some((created, changed)));
            }
        }
        // Every tile's status follows its <video> media events (see `refresh`).
        // The own preview also rebinds to the live capture when it changes (a
        // camera switch), woken by that change rather than by a timer.
        if !own {
            return;
        }
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    let mut shown: Option<String> = None;
                    loop {
                        let seen = video::local_revision();
                        let playing = {
                            // Self-preview follows the live capture, including a
                            // camera switch; nothing is sent for it.
                            let track = video::local_track(watch.kind);
                            let id = track.as_ref().map(MediaStreamTrack::id);
                            if id != shown {
                                shown = id;
                                let stream = track.and_then(|t| {
                                    MediaStream::new_with_tracks(&js_sys::Array::of1(&t)).ok()
                                });
                                element.set_src_object(stream.as_ref());
                                if let Ok(promise) = element.play() {
                                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                                }
                            }
                            shown.is_some()
                        };
                        let ready =
                            playing && element.ready_state() >= HtmlMediaElement::HAVE_CURRENT_DATA;
                        let next = if ready {
                            String::new()
                        } else {
                            "Waiting for video…".into()
                        };
                        if message.try_get_untracked().is_none() {
                            break;
                        }
                        if message.with_untracked(|m| *m != next) {
                            message.set(next);
                        }
                        // Readiness then follows media events; 5 s is only a
                        // safety net for a missed capture change.
                        video::local_change_or(seen, 5000).await;
                    }
                },
                registration,
            )
            .await;
        });
    });
    // Remote tiles: bound to a received track and showing a frame. The own
    // preview: showing the live local capture.
    let refresh = move || {
        let Some(element) = video_node.get_untracked() else {
            return;
        };
        let bound = if own {
            video::local_track(watch.kind).is_some()
        } else {
            view_id.get_value().is_some_and(video::bound)
        };
        let ready = bound && element.ready_state() >= HtmlMediaElement::HAVE_CURRENT_DATA;
        let next = if ready { "" } else { "Waiting for video…" };
        if message.with_untracked(|m| m != next) {
            message.set(next.into());
        }
    };
    let label = if watch.kind == MediaKind::Camera {
        if own { "Your camera" } else { "Live video" }
    } else if own {
        "Your screen"
    } else {
        "Live video"
    };
    view! {
        <div class="stream-player"><video node_ref=video_node autoplay muted playsinline aria-label=move || crate::i18n::t(label)
            on:loadeddata=move |_| refresh() on:playing=move |_| refresh() on:waiting=move |_| refresh()
            on:stalled=move |_| refresh() on:emptied=move |_| refresh()/>
            <Show when=move ||!message.get().is_empty()><p class="stream-player-status" role="status">{move ||crate::i18n::message(message.get())}</p></Show>
        </div>
    }
}
