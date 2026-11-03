//! Camera choice UI for the desktop app and the `/app` browser client: the
//! voice-bar quick switch, the missing-camera notice and the settings card with
//! a local preview. Enumeration, persistence and fallback are in `cameras.rs`.
use super::Ui;
use super::icons::{Glyph, Icon};
use crate::cameras;
use leptos::prelude::*;
use thiscord_frontend::{browser_video::Facing, camera_choice};
use wasm_bindgen::JsCast;
use web_sys::{MediaStream, MediaStreamTrack, MediaStreamTrackState};

fn choices() -> Vec<camera_choice::Choice> {
    camera_choice::choices(&cameras::list(), cameras::saved().as_ref())
}
fn selected() -> String {
    cameras::saved().map(|s| s.id).unwrap_or_default()
}
fn choice_label(choice: &camera_choice::Choice) -> String {
    let name = cameras::label(&choice.label, choice.index);
    if choice.missing {
        thiscord_frontend::localized_format!(crate::i18n::language(), "{} (not connected)", name)
    } else {
        name
    }
}
/// Moves focus within a menu: `None` first, `Some(true)` next, `Some(false)`
/// previous, wrapping around.
fn focus_item(menu: &web_sys::Element, step: Option<bool>) {
    let Ok(items) = menu.query_selector_all("[role^='menuitem']") else {
        return;
    };
    let items: Vec<web_sys::HtmlElement> = (0..items.length())
        .filter_map(|i| items.item(i)?.dyn_into().ok())
        .collect();
    if items.is_empty() {
        return;
    }
    let focused = document().active_element();
    let current = items
        .iter()
        .position(|item| focused.as_ref().is_some_and(|f| f == item.unchecked_ref()));
    let next = match (step, current) {
        (None, _) | (Some(true), None) => 0,
        (Some(false), None) => items.len() - 1,
        (Some(true), Some(i)) => (i + 1) % items.len(),
        (Some(false), Some(i)) => (i + items.len() - 1) % items.len(),
    };
    let _ = items[next].focus();
}

/// Chevron next to the camera button: pick a camera, or flip front/back on
/// phones. Selecting saves the choice and switches a running camera in place.
#[component]
pub(super) fn CameraMenu(
    ui: Ui,
    /// Front/back flipping (touch browsers only).
    #[prop(default = None)]
    flip: Option<Callback<()>>,
) -> impl IntoView {
    cameras::watch();
    let open = RwSignal::new(false);
    let button = NodeRef::<leptos::html::Button>::new();
    let menu = NodeRef::<leptos::html::Div>::new();
    let options = Memo::new(move |_| choices());
    let flipping = move || flip.is_some() && cameras::call_camera();
    let visible = move || options.with(|c| c.len() > 1) || flipping();
    let close = move |refocus: bool| {
        open.set(false);
        if refocus && let Some(button) = button.get_untracked() {
            let _ = button.focus();
        }
    };
    let pick = move |id: Option<String>| {
        cameras::select(id);
        close(true);
    };
    Effect::new(move |_| {
        if open.get()
            && let Some(menu) = menu.get()
        {
            let checked = menu.query_selector("[aria-checked='true']").ok().flatten();
            match checked.and_then(|c| c.dyn_into::<web_sys::HtmlElement>().ok()) {
                Some(item) => {
                    let _ = item.focus();
                }
                None => focus_item(&menu, None),
            }
        }
    });
    let outside = window_event_listener(leptos::ev::pointerdown, move |_| open.set(false));
    on_cleanup(move || outside.remove());
    let keydown = move |e: web_sys::KeyboardEvent| {
        let Some(list) = menu.get_untracked() else {
            return;
        };
        let step = match e.key().as_str() {
            "Escape" => {
                e.prevent_default();
                e.stop_propagation();
                close(true);
                return;
            }
            "Tab" => {
                close(false);
                return;
            }
            "ArrowDown" => Some(true),
            "ArrowUp" => Some(false),
            "Home" => None,
            "End" => {
                e.prevent_default();
                focus_item(&list, None);
                focus_item(&list, Some(false));
                return;
            }
            _ => return,
        };
        e.prevent_default();
        focus_item(&list, step);
    };
    view! {
        <Show when=visible>
            <div class="camera-switch" on:pointerdown=move |e|e.stop_propagation() on:keydown=keydown>
                <button node_ref=button class="icon-button camera-chevron" aria-label=move || crate::i18n::t("Choose camera") data-tip=move || crate::i18n::t("Choose camera")
                    aria-haspopup="menu" aria-expanded=move ||open.get().to_string() on:click=move |_|open.update(|o| *o = !*o)
                    on:keydown=move |e:web_sys::KeyboardEvent|if matches!(e.key().as_str(),"ArrowUp"|"ArrowDown") && !open.get_untracked() {e.prevent_default();open.set(true);}>
                    <Icon kind=Glyph::Chevron/>
                </button>
                <Show when=move ||open.get()>
                    <div node_ref=menu class="camera-menu" role="menu" aria-label=move || crate::i18n::t("Cameras")>
                        <button role="menuitemradio" class="stream-choice" tabindex="-1" aria-checked=move ||selected().is_empty().to_string() on:click=move |_|pick(None)>{move || crate::i18n::t("System default")}</button>
                        <For each=move ||options.get() key=|c|(c.id.clone(),c.missing,c.label.clone()) children=move |choice| {
                            let id = choice.id.clone();
                            let checked = choice.id.clone();
                            let label = choice_label(&choice);
                            view!{<button role="menuitemradio" class="stream-choice" tabindex="-1" aria-checked=move ||(selected()==checked).to_string() on:click=move |_|pick(Some(id.clone()))>{label}</button>}
                        }/>
                        <Show when=flipping>
                            <button role="menuitem" class="stream-choice" tabindex="-1" on:click=move |_|{close(true);if let Some(flip)=flip{flip.run(());}}>{move || crate::i18n::t("Flip front/back camera")}</button>
                        </Show>
                        <button role="menuitem" class="stream-choice" tabindex="-1" on:click=move |_|{close(false);ui.page.set("audio");}>{move || crate::i18n::t("Camera settings")}</button>
                    </div>
                </Show>
            </div>
        </Show>
    }
}

/// One-time notice that the saved camera is missing and the default is used.
#[component]
pub(super) fn CameraNotice() -> impl IntoView {
    view! {
        <Show when=move ||!cameras::notice().is_empty()>
            <p class="camera-notice mt-2 flex items-start gap-2 text-xs text-amber-200" role="status">
                <span class="min-w-0 flex-1">{cameras::notice}</span>
                <button class="shrink-0 underline" on:click=move |_|cameras::dismiss_notice()>{move || crate::i18n::t("Dismiss")}</button>
            </p>
        </Show>
    }
}

/// Settings card: choose the camera and preview it. Outside a call camera the
/// preview opens its own capture; while the call camera is on it shows that.
#[component]
pub(super) fn CameraSettings(ui: Ui) -> impl IntoView {
    cameras::watch();
    let browser = super::audio::browser_voice(ui);
    let video_node = NodeRef::<leptos::html::Video>::new();
    let preview = StoredValue::new_local(None::<MediaStream>);
    let generation = StoredValue::new(0u32);
    let previewing = RwSignal::new(false);
    let starting = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let release = move || {
        generation.try_update_value(|g| *g = g.wrapping_add(1));
        if let Some(Some(stream)) = preview.try_update_value(Option::take) {
            cameras::stop_stream(&stream);
        }
    };
    let stop = move || {
        release();
        if let Some(video) = video_node.get_untracked() {
            video.set_src_object(None);
        }
        previewing.set(false);
        starting.set(false);
    };
    let start = move || {
        stop();
        previewing.set(true);
        starting.set(true);
        error.set(String::new());
        let current = generation.get_value();
        leptos::task::spawn_local(async move {
            let result = cameras::open(browser.then_some(Facing::User), true).await;
            if generation.try_get_value() != Some(current) {
                if let Ok(stream) = result {
                    cameras::stop_stream(&stream);
                }
                return;
            }
            starting.set(false);
            match result {
                Ok(stream) => {
                    if let Some(video) = video_node.get_untracked() {
                        video.set_src_object(Some(&stream));
                        let _ = video.play();
                    }
                    preview.set_value(Some(stream));
                }
                Err(e) => {
                    previewing.set(false);
                    error.set(cameras::error_message(&e).into());
                }
            }
        });
    };
    on_cleanup(release);
    // A new selection restarts a running preview with that camera.
    Effect::new(move |previous: Option<u32>| {
        let revision = cameras::revision();
        if previous.is_some_and(|p| p != revision) && previewing.get_untracked() {
            start();
        }
        revision
    });
    // The call camera takes over; its own preview is shown instead.
    Effect::new(move |_| {
        if cameras::call_camera() && previewing.get_untracked() {
            stop();
        }
    });
    // An unplugged or revoked camera ends the preview track.
    Effect::new(move |_| {
        cameras::list();
        let ended = preview.with_value(|stream| {
            stream.as_ref().is_some_and(|stream| {
                stream.get_video_tracks().iter().any(|t| {
                    t.unchecked_into::<MediaStreamTrack>().ready_state()
                        == MediaStreamTrackState::Ended
                })
            })
        });
        if ended {
            stop();
            error.set("The camera preview stopped. Check that the camera is connected.".into());
        }
    });
    let own = Signal::derive(move || ui.account.get().map(|a| a.id));
    view! {
        <section class="settings-card" aria-labelledby="camera-settings-title">
            <h3 id="camera-settings-title">{move || crate::i18n::t("Camera")}</h3>
            <p class="settings-hint">{move || crate::i18n::t("Saved on this device. A new choice applies immediately, including to a camera that is on in a call.")}</p>
            <label class="block space-y-2"><span>{move || crate::i18n::t("Camera")}</span>
                <select class="w-full rounded bg-slate-900 p-2" prop:value=selected on:change=move |e|{let v=event_target_value(&e);cameras::select((!v.is_empty()).then_some(v));}>
                    <option value="" selected=move ||selected().is_empty()>{move || crate::i18n::t("System default")}</option>
                    {move ||choices().into_iter().map(|choice|{
                        let id = choice.id.clone();
                        let current = id.clone();
                        view!{<option value=id selected=move ||selected()==current>{choice_label(&choice)}</option>}
                    }).collect_view()}
                </select>
            </label>
            <Show when=move ||cameras::list().needs_permission()>
                <p class="settings-hint">{move || crate::i18n::t("Camera names appear after you allow camera access. Preview your camera to allow it.")}</p>
            </Show>
            <Show when=move ||cameras::listed() && cameras::list().present == 0>
                <p class="settings-hint">{move || crate::i18n::t("No camera found. Connect a webcam or enable a virtual camera.")}</p>
            </Show>
            <CameraNotice/>
            <div class="camera-preview">
                <Show when=move ||cameras::call_camera() fallback=move ||view!{
                    <video node_ref=video_node class:hidden=move ||!previewing.get() autoplay muted playsinline aria-label=move || crate::i18n::t("Camera preview")></video>
                    <Show when=move ||!previewing.get() || starting.get()>
                        <p class="camera-preview-status" role="status">{move ||if starting.get(){crate::i18n::t("Starting camera…")}else{crate::i18n::t("Camera preview is off")}}</p>
                    </Show>
                }>
                    {move ||if browser {
                        own.get().map(|owner|view!{<super::browser_media::BrowserVideo watch=thiscord_shared::screen::Watch{kind:thiscord_shared::voice::MediaKind::Camera,slot:0,owner,epoch:0,viewer:0} own=true/>}.into_any())
                    } else {
                        Some(view!{<super::camera::LocalCamera/>}.into_any())
                    }}
                </Show>
            </div>
            <Show when=move ||cameras::call_camera() fallback=move ||view!{
                <div class="settings-actions">
                    <button class="rounded bg-white/10 px-4 py-2 disabled:opacity-50" aria-pressed=move ||previewing.get().to_string() on:click=move |_|if previewing.get_untracked(){stop();}else{start();}>
                        {move ||if previewing.get(){crate::i18n::t("Stop preview")}else{crate::i18n::t("Preview camera")}}
                    </button>
                </div>
            }>
                <p class="settings-hint">{move || crate::i18n::t("Your camera is on in the call. This preview is what others see.")}</p>
            </Show>
            <Show when=move ||!error.get().is_empty()><p class="text-sm text-red-300" role="alert">{move ||crate::i18n::message(error.get())}</p></Show>
        </section>
    }
}
