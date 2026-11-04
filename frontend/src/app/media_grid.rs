//! The call media area: cameras and screen shares placed by their real shape,
//! with focus, drag-and-drop reordering, hide/restore and live quality.
//! Placement math lives in `thiscord_frontend::media_layout`.
use super::Ui;
use super::icons::{Glyph, Icon};
use leptos::prelude::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use thiscord_frontend::{
    localized_format,
    media_layout::{self, Rect},
};
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    screen::Watch,
    voice::{MediaKind, Participant},
};
use wasm_bindgen::{JsCast, prelude::Closure};

/// One stream generation: a restarted share is a new tile.
type Key = (AccountId, MediaKind, u32);
type Tile = (Participant, MediaKind, u32);

fn key_of((member, kind, epoch): &Tile) -> Key {
    (member.account_id, *kind, *epoch)
}

fn key_text(key: Key) -> String {
    let kind = if key.1 == MediaKind::Camera {
        "camera"
    } else {
        "screen"
    };
    format!("{}-{kind}-{}", key.0, key.2)
}

/// "Screen share from Ada" / "Camera from Ada".
fn tile_label(name: &str, kind: MediaKind) -> String {
    let language = crate::i18n::language();
    if kind == MediaKind::Camera {
        localized_format!(language, "Camera from {}", name)
    } else {
        localized_format!(language, "Screen share from {}", name)
    }
}

#[derive(Clone, PartialEq, Default)]
struct Arrangement {
    rects: Vec<(Key, Rect)>,
    stage: Option<Key>,
    height: f64,
    width: f64,
    scrolls: bool,
}

/// Frames counted at the last sample, for measuring the received frame rate.
type Samples = Rc<RefCell<HashMap<Key, (u32, f64)>>>;

static NEXT_GRID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn observe_size(element: &web_sys::Element, callback: impl FnMut() + 'static) {
    let callback = Closure::<dyn FnMut()>::new(callback);
    if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
        observer.observe(element);
        let resource = StoredValue::new_local((observer, callback));
        on_cleanup(move || resource.with_value(|(observer, _)| observer.disconnect()));
    }
}

fn now() -> f64 {
    window().performance().map(|p| p.now()).unwrap_or_default()
}

fn focus_selector(selector: &str) {
    if let Some(element) = document()
        .query_selector(selector)
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = element.focus();
    }
}

#[component]
pub(super) fn ScreenViewer(ui: Ui, guild: GuildId, channel: ChannelId) -> impl IntoView {
    let grid_id = NEXT_GRID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hint_id = move || format!("media-grid-hint-{grid_id}");
    let hidden = RwSignal::new(Vec::<Key>::new());
    let order = RwSignal::new(Vec::<Key>::new());
    let focus = RwSignal::new(None::<Key>);
    let menu = RwSignal::new(None::<Key>);
    let expanded = RwSignal::new(None::<Key>);
    let dragging = RwSignal::new(None::<Key>);
    let drop_stage = RwSignal::new(false);
    let size = RwSignal::new((0.0f64, 0.0f64));
    let dims = RwSignal::new(HashMap::<Key, (u32, u32)>::new());
    let rates = RwSignal::new(HashMap::<Key, f64>::new());
    let announcement = RwSignal::new(String::new());
    let last_move = StoredValue::new(0.0f64);
    let stage = NodeRef::<leptos::html::Div>::new();
    let announce = move |text: String| announcement.set(text);

    let tiles = Memo::new(move |_| {
        let voice = ui.voice.get();
        if !voice.connected || voice.channel_id != Some(channel) {
            return vec![];
        }
        voice
            .participants
            .into_iter()
            .flat_map(|m| {
                let mut tiles = vec![];
                if m.camera_active {
                    tiles.push((m.clone(), MediaKind::Camera, m.camera_epoch));
                }
                if m.sharing_screen {
                    tiles.push((m.clone(), MediaKind::ScreenVideo, m.screen_epoch));
                }
                tiles
            })
            .collect::<Vec<Tile>>()
    });
    // The user's order first; streams that start later join at the end.
    let visible = Memo::new(move |_| {
        let tiles = tiles.get();
        let hidden = hidden.get();
        let order = order.get();
        let mut keys: Vec<Key> = order
            .iter()
            .copied()
            .filter(|k| tiles.iter().any(|t| key_of(t) == *k))
            .collect();
        keys.extend(tiles.iter().map(key_of).filter(|k| !order.contains(k)));
        keys.into_iter()
            .filter(|k| !hidden.contains(k))
            .filter_map(|k| tiles.iter().find(|t| key_of(t) == k).cloned())
            .collect::<Vec<Tile>>()
    });
    let visible_keys =
        Memo::new(move |_| visible.with(|v| v.iter().map(key_of).collect::<Vec<_>>()));
    let hidden_tiles = Memo::new(move |_| {
        let tiles = tiles.get();
        hidden
            .get()
            .into_iter()
            .filter_map(|k| tiles.iter().find(|t| key_of(t) == k).cloned())
            .collect::<Vec<Tile>>()
    });
    Effect::new(move |_| {
        let keys: Vec<Key> = tiles.with(|t| t.iter().map(key_of).collect());
        let mut next = order.get_untracked();
        next.retain(|k| keys.contains(k));
        next.extend(
            keys.iter()
                .filter(|k| !next.contains(k))
                .copied()
                .collect::<Vec<_>>(),
        );
        if next != order.get_untracked() {
            order.set(next);
        }
        if hidden.with_untracked(|h| h.iter().any(|k| !keys.contains(k))) {
            hidden.update(|h| h.retain(|k| keys.contains(k)));
        }
        if dims.with_untracked(|d| d.keys().any(|k| !keys.contains(k))) {
            dims.update(|d| d.retain(|k, _| keys.contains(k)));
            rates.update(|r| r.retain(|k, _| keys.contains(k)));
        }
        if expanded.get_untracked().is_some_and(|k| !keys.contains(&k)) {
            expanded.set(None);
        }
    });
    Effect::new(move |_| {
        let keys = visible_keys.get();
        if focus.get_untracked().is_some_and(|k| !keys.contains(&k)) {
            focus.set(None);
        }
        if menu.get_untracked().is_some_and(|k| !keys.contains(&k)) {
            menu.set(None);
        }
    });

    let arrangement = Memo::new(move |_| {
        let (width, height) = size.get();
        let keys = visible_keys.get();
        let aspects: Vec<f64> = dims.with(|d| {
            keys.iter()
                .map(|k| match d.get(k) {
                    Some((w, h)) if *w > 0 && *h > 0 => *w as f64 / *h as f64,
                    _ => media_layout::DEFAULT_ASPECT,
                })
                .collect()
        });
        let gap = if width < 600.0 { 8.0 } else { 12.0 };
        let focused = focus
            .get()
            .and_then(|f| keys.iter().position(|k| *k == f))
            .filter(|_| keys.len() > 1);
        let placement = match focused {
            Some(i) => media_layout::focus(width, height, gap, &aspects, i),
            None => media_layout::grid(width, height, gap, &aspects),
        };
        Arrangement {
            rects: keys.iter().copied().zip(placement.tiles).collect(),
            stage: focused.map(|i| keys[i]),
            height: placement.height,
            width,
            scrolls: placement.scrolls,
        }
    });

    // The media area's size drives the layout.
    Effect::new(move |_| {
        let Some(element) = stage.get() else {
            return;
        };
        let element: web_sys::HtmlElement = element.clone().into();
        let measure = {
            let element = element.clone();
            move || {
                let next = (
                    element.client_width() as f64,
                    element.client_height() as f64,
                );
                if size.try_get_untracked().is_some_and(|s| s != next) {
                    size.set(next);
                }
            }
        };
        measure();
        observe_size(&element, measure);
    });

    // Sample each player's decoded size and frame rate.
    let samples: Samples = Rc::default();
    let sampler = set_interval_with_handle(
        move || {
            let Some(root) = stage.get_untracked() else {
                return;
            };
            let Some(keys) = visible_keys.try_get_untracked() else {
                return;
            };
            let at = now();
            let mut samples = samples.borrow_mut();
            samples.retain(|k, _| keys.contains(k));
            for key in keys {
                let Some(video) = root
                    .query_selector(&format!("[data-media-key='{}'] video", key_text(key)))
                    .ok()
                    .flatten()
                    .and_then(|v| v.dyn_into::<web_sys::HtmlVideoElement>().ok())
                else {
                    continue;
                };
                let size = (video.video_width(), video.video_height());
                if size.0 > 0 && dims.with_untracked(|d| d.get(&key) != Some(&size)) {
                    dims.update(|d| {
                        d.insert(key, size);
                    });
                }
                let frames = video.get_video_playback_quality().total_video_frames();
                match samples.get(&key).copied() {
                    Some((before, then)) if at - then >= 950.0 => {
                        let fps = frames.saturating_sub(before) as f64 * 1000.0 / (at - then);
                        samples.insert(key, (frames, at));
                        let changed = rates.with_untracked(|r| {
                            r.get(&key).is_none_or(|old| (old - fps).abs() >= 0.5)
                        });
                        if changed {
                            rates.update(|r| {
                                r.insert(key, fps);
                            });
                        }
                    }
                    Some(_) => {}
                    None => {
                        samples.insert(key, (frames, at));
                    }
                }
            }
        },
        std::time::Duration::from_millis(500),
    )
    .ok();

    let tile_name = move |key: Key| {
        tiles.with_untracked(|t| {
            t.iter()
                .find(|t| key_of(t) == key)
                .map(|(m, kind, _)| tile_label(m.display_name(), *kind))
                .unwrap_or_default()
        })
    };
    let exit_full = move || {
        if expanded.get_untracked().is_some() {
            expanded.set(None);
            if document().fullscreen_element().is_some() {
                document().exit_fullscreen();
            }
        }
    };
    let set_focus = move |next: Option<Key>| {
        let previous = focus.get_untracked();
        focus.set(next);
        // A scrolled phone list would otherwise hide the new stage.
        if let Some(stage) = stage.get_untracked() {
            stage.set_scroll_top(0);
        }
        match next {
            Some(key) => announce(localized_format!(
                crate::i18n::language(),
                "Focused: {}",
                tile_name(key)
            )),
            None if previous.is_some() => announce(crate::i18n::t("Showing all streams").into()),
            None => {}
        }
    };
    // Moves `key` to where `target` is in the visible order.
    let move_to = move |key: Key, target: Key| {
        if key == target {
            return;
        }
        order.update(|o| {
            for k in visible_keys.get_untracked() {
                if !o.contains(&k) {
                    o.push(k);
                }
            }
            if let (Some(from), Some(to)) = (
                o.iter().position(|k| *k == key),
                o.iter().position(|k| *k == target),
            ) {
                media_layout::move_item(o, from, to);
            }
        });
    };
    let announce_position = move |key: Key| {
        let keys = visible_keys.get_untracked();
        if let Some(index) = keys.iter().position(|k| *k == key) {
            announce(localized_format!(
                crate::i18n::language(),
                "{}: position {} of {}",
                tile_name(key),
                index + 1,
                keys.len()
            ));
        }
    };
    let move_by = move |key: Key, delta: isize| {
        let keys = visible_keys.get_untracked();
        let Some(index) = keys.iter().position(|k| *k == key) else {
            return;
        };
        let Some(target) = index
            .checked_add_signed(delta)
            .and_then(|i| keys.get(i).copied())
        else {
            return;
        };
        move_to(key, target);
        announce_position(key);
    };
    let restore = move |key: Key| {
        hidden.update(|h| h.retain(|k| *k != key));
        announce(localized_format!(
            crate::i18n::language(),
            "Showing {}",
            tile_name(key)
        ));
    };
    let close_menu = move |return_focus: bool| {
        if let Some(key) = menu.get_untracked() {
            menu.set(None);
            if return_focus {
                focus_selector(&format!(
                    "[data-media-key='{}'] .media-tile-more",
                    key_text(key)
                ));
            }
        }
    };

    let escape = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() != "Escape" {
            return;
        }
        // Handled here, so Escape does not also mark the channel read.
        if menu.with_untracked(Option::is_some) {
            e.prevent_default();
            close_menu(true);
        } else if expanded.with_untracked(Option::is_some) {
            e.prevent_default();
            expanded.set(None);
        } else if focus.with_untracked(Option::is_some)
            && document()
                .active_element()
                .is_some_and(|a| a.closest(".media-stage").ok().flatten().is_some())
        {
            e.prevent_default();
            set_focus(None);
        }
    });
    let fullscreen = window_event_listener(leptos::ev::fullscreenchange, move |_| {
        if document().fullscreen_element().is_none() {
            expanded.set(None);
        }
    });
    let outside = window_event_listener(leptos::ev::pointerdown, move |_| close_menu(false));
    on_cleanup(move || {
        escape.remove();
        fullscreen.remove();
        outside.remove();
        if let Some(handle) = sampler {
            handle.clear();
        }
    });
    // Opening a menu moves keyboard focus to its first item.
    Effect::new(move |_| {
        if let Some(key) = menu.get() {
            leptos::task::spawn_local(async move {
                leptos::task::tick().await;
                focus_selector(&format!(
                    "[data-media-key='{}'] [role=menuitem]",
                    key_text(key)
                ));
            });
        }
    });

    let count = move || {
        let n = visible.with(Vec::len);
        if n == 1 {
            crate::i18n::t("1 stream").to_owned()
        } else {
            localized_format!(crate::i18n::language(), "{} streams", n)
        }
    };
    let several = move || visible.with(|v| v.len() > 1);
    let focused = move || arrangement.with(|a| a.stage.is_some());

    view! {<div class="channel-streams">
        <Show when=move ||!visible.get().is_empty() || !hidden_tiles.get().is_empty()>
            <div class="media-bar">
                <p class="media-bar-count">{count}</p>
                <Show when=move ||!hidden_tiles.get().is_empty()>
                    <div class="media-bar-hidden" role="group" aria-label=move ||crate::i18n::t("Hidden streams")>
                        <span class="media-bar-label" aria-hidden="true">{move ||crate::i18n::t("Hidden")}</span>
                        <For each=move ||hidden_tiles.get() key=key_of children=move |tile| {
                            let key = key_of(&tile);
                            let label = tile_label(tile.0.display_name(), tile.1);
                            let short = tile.0.display_name().to_owned();
                            let camera = tile.1 == MediaKind::Camera;
                            view!{<button type="button" class="media-chip" data-tip=move ||crate::i18n::t("Show again")
                                aria-label=move ||localized_format!(crate::i18n::language(), "Show {}", label)
                                on:click=move |_|restore(key)>
                                <Icon kind=if camera {Glyph::Camera} else {Glyph::Screen}/><span class="truncate">{short}</span><Icon kind=Glyph::Eye/>
                            </button>}
                        }/>
                        <Show when=move ||hidden_tiles.with(|h| h.len() > 1)>
                            <button type="button" class="media-chip media-chip-plain" on:click=move |_|{hidden.set(vec![]);announce(crate::i18n::t("Showing all streams").into());}>{move ||crate::i18n::t("Show all")}</button>
                        </Show>
                    </div>
                </Show>
                <span class="media-bar-hint" id=hint_id() class:hidden=move ||!several()>{move ||crate::i18n::t("Click to focus · Drag to rearrange")}</span>
                <Show when=focused>
                    <button type="button" class="media-bar-grid" on:click=move |_|set_focus(None)><Icon kind=Glyph::Grid/>{move ||crate::i18n::t("Back to grid")}</button>
                </Show>
            </div>
        </Show>
        <p class="sr-only" role="status" aria-live="polite" aria-atomic="true">{move ||announcement.get()}</p>
        <Show when=move ||visible.get().is_empty()><p class="voice-channel-empty">{move || {
            let voice = ui.voice.get();
            if !voice.connected || voice.channel_id != Some(channel) { crate::i18n::t("Join this call to see cameras and screen shares.") }
            else if !tiles.get().is_empty() { crate::i18n::t("All streams are hidden. Choose one above to show it again.") }
            else { crate::i18n::t("No cameras or screen shares are active.") }
        }}</p></Show>
        <div node_ref=stage class="media-stage" class:empty=move ||visible.get().is_empty() class:scrolls=move ||arrangement.with(|a|a.scrolls) class:dragging=move ||dragging.get().is_some() class:focus-mode=focused>
            <div class="media-stage-content" style:height=move ||arrangement.with(|a| if a.scrolls {format!("{:.0}px", a.height)} else {"100%".into()})>
            <For each=move ||visible.get() key=|t|(t.0.account_id,t.0.slot,t.1,t.2) children=move |(member,kind,epoch)| {
                let owner=member.account_id;
                let key=(owner,kind,epoch);
                let name=member.display_name().to_owned();
                let label=tile_label(&name,kind);
                let own=ui.account.get_untracked().is_some_and(|a|a.id==owner);
                let camera=kind==MediaKind::Camera;
                let watch=Watch{kind,slot:member.slot,owner,epoch,viewer:0};
                let figure=NodeRef::<leptos::html::Figure>::new();
                let rect=Memo::new(move |_|arrangement.with(|a|a.rects.iter().find(|(k,_)|*k==key).map(|(_,r)|*r)));
                let on_stage=Memo::new(move |_|arrangement.with(|a|a.stage==Some(key)));
                let is_expanded=move ||expanded.get()==Some(key);
                let placed=move |f: fn(Rect)->String| move ||if is_expanded() {String::new()} else {rect.get().map(f).unwrap_or_default()};
                let full=move || {
                    menu.set(None);
                    if expanded.get_untracked()==Some(key) {exit_full();}
                    else {expanded.set(Some(key));if let Some(node)=figure.get_untracked(){let _=node.request_fullscreen();}}
                };
                let toggle_focus=move || {
                    menu.set(None);
                    if visible_keys.with_untracked(|v|v.len()<=1) {full();return;}
                    set_focus(if focus.get_untracked()==Some(key) {None} else {Some(key)});
                };
                let open_menu=move || {ui.context_menu.set(None);menu.set(Some(key));};
                let index=Memo::new(move |_|visible_keys.with(|v|v.iter().position(|k|*k==key)));
                let first=move ||index.get()==Some(0);
                let last=move ||index.get().is_some_and(|i|visible_keys.with(|v|i+1==v.len()));
                let menu_up=move ||arrangement.with(|a|rect.get().is_some_and(|r|r.y+r.h/2.0>a.height/2.0 && r.h<420.0));
                let menu_start=move ||arrangement.with(|a|rect.get().is_some_and(|r|r.x+r.w<320.0 && a.width>r.x+r.w+8.0));
                let quality=Memo::new(move |_| {
                    let (w,h)=dims.with(|d|d.get(&key).copied())?;
                    let fps=rates.with(|r|r.get(&key).copied()).unwrap_or_default();
                    let resolution=media_layout::resolution_label(w,h);
                    let nominal=media_layout::nominal_fps(fps);
                    let language=crate::i18n::language();
                    let short=if nominal==0 {resolution} else {localized_format!(language, "{} · {} fps", resolution, nominal)};
                    let detail=if own {localized_format!(language, "Your capture: {} × {} at {} frames per second", w, h, format!("{fps:.1}"))}
                        else {localized_format!(language, "Receiving {} × {} at {} frames per second", w, h, format!("{fps:.1}"))};
                    Some((short,detail))
                });
                let hint=hint_id();
                let label_menu=label.clone();
                let label_heading=label.clone();
                view!{<figure node_ref=figure class="media-tile" data-media-key=key_text(key)
                    class:expanded=is_expanded class:stage=move ||on_stage.get() class:unplaced=move ||rect.get().is_none()
                    class:dragging=move ||dragging.get()==Some(key) class:drop-stage=move ||on_stage.get() && drop_stage.get()
                    class:menu-open=move ||menu.get()==Some(key)
                    style:left=placed(|r|format!("{:.1}px",r.x)) style:top=placed(|r|format!("{:.1}px",r.y))
                    style:width=placed(|r|format!("{:.1}px",r.w)) style:height=placed(|r|format!("{:.1}px",r.h))
                    draggable=move ||if menu.get()==Some(key) || is_expanded() || on_stage.get() || !several() {"false"} else {"true"}
                    on:contextmenu=move |e|{e.prevent_default();e.stop_propagation();open_menu();}
                    on:dragstart=move |e: web_sys::DragEvent|{
                        if let Some(transfer)=e.data_transfer(){let _=transfer.set_data("text/plain",&name);transfer.set_effect_allowed("move");}
                        menu.set(None);dragging.set(Some(key));
                    }
                    on:dragover=move |e: web_sys::DragEvent|{
                        let Some(source)=dragging.get_untracked() else {return;};
                        e.prevent_default();
                        if let Some(transfer)=e.data_transfer(){transfer.set_drop_effect("move");}
                        if source==key {return;}
                        if on_stage.get_untracked() {if !drop_stage.get_untracked(){drop_stage.set(true);} return;}
                        // Let the previous move finish animating before the next.
                        if now()-last_move.get_value()<260.0 {return;}
                        last_move.set_value(now());
                        move_to(source,key);
                    }
                    on:dragleave=move |e: web_sys::DragEvent|{
                        let inside=e.related_target().and_then(|t|t.dyn_into::<web_sys::Node>().ok()).zip(figure.get_untracked()).is_some_and(|(t,f)|f.contains(Some(&t)));
                        if on_stage.get_untracked() && !inside {drop_stage.set(false);}
                    }
                    on:drop=move |e: web_sys::DragEvent|{
                        e.prevent_default();
                        if let Some(source)=dragging.get_untracked() && on_stage.get_untracked() && source!=key {set_focus(Some(source));}
                        drop_stage.set(false);
                    }
                    on:dragend=move |_|{
                        drop_stage.set(false);
                        if let Some(source)=dragging.get_untracked(){dragging.set(None);if focus.get_untracked()!=Some(source){announce_position(source);}}
                    }>
                    <div class="media-tile-content" role="button" tabindex="0" aria-label=label.clone() aria-describedby=hint
                        aria-pressed=move ||several().then(||on_stage.get().to_string())
                        on:click=move |_|toggle_focus()
                        on:keydown=move |e|{
                            let key_name=e.key();
                            let plain=!e.alt_key()&&!e.ctrl_key()&&!e.meta_key();
                            match key_name.as_str() {
                                "Enter"|" " if plain && !e.shift_key() => {e.prevent_default();toggle_focus();}
                                "f"|"F" if plain => {e.prevent_default();full();}
                                "ArrowLeft"|"ArrowUp" if e.alt_key() => {e.prevent_default();move_by(key,-1);}
                                "ArrowRight"|"ArrowDown" if e.alt_key() => {e.prevent_default();move_by(key,1);}
                                "ContextMenu" => {e.prevent_default();open_menu();}
                                "F10" if e.shift_key() => {e.prevent_default();open_menu();}
                                _ => {}
                            }
                        }>
                        <Show when=move ||!super::audio::media_deafened(ui) fallback=||view!{<div class="stream-placeholder">{move || crate::i18n::t("Undeafen to view media")}</div>}>
                            {move || if super::audio::browser_voice(ui) {view!{<super::browser_media::BrowserVideo watch=watch own=own/>}.into_any()}
                            else if own && camera {view!{<super::camera::LocalCamera/>}.into_any()}
                            else {view!{<super::screen::ScreenVideo watch=watch/>}.into_any()}}
                        </Show>
                    </div>
                    <figcaption class="media-tile-caption"><Icon kind=if camera {Glyph::Camera} else {Glyph::Screen}/><span class="truncate">{member.display_name().to_owned()}</span></figcaption>
                    <div class="media-tile-actions">
                        <Show when=several>
                            <button type="button" class="media-tile-button media-tile-focus" aria-pressed=move ||on_stage.get().to_string()
                                aria-label=move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}
                                data-tip=move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}
                                on:click=move |_|toggle_focus()>{move ||view!{<Icon kind=if on_stage.get() {Glyph::Grid} else {Glyph::Spotlight}/>}}</button>
                        </Show>
                        <button type="button" class="media-tile-button media-tile-full"
                            aria-label=move ||if is_expanded() {crate::i18n::t("Exit full screen")} else {crate::i18n::t("Full screen")}
                            data-tip=move ||if is_expanded() {crate::i18n::t("Exit full screen")} else {crate::i18n::t("Full screen")}
                            on:click=move |_|full()>{move ||view!{<Icon kind=if is_expanded() {Glyph::Shrink} else {Glyph::Expand}/>}}</button>
                        <button type="button" class="media-tile-button media-tile-more" aria-haspopup="menu" aria-expanded=move ||(menu.get()==Some(key)).to_string()
                            aria-label=move ||crate::i18n::t("Stream options") data-tip=move ||crate::i18n::t("Stream options")
                            on:pointerdown=move |e|e.stop_propagation()
                            on:click=move |_|if menu.get_untracked()==Some(key) {menu.set(None);} else {open_menu();}><Icon kind=Glyph::More/></button>
                    </div>
                    <Show when=move ||quality.get().is_some()>
                        <span class="media-quality" tabindex="0" role="note"
                            aria-label=move ||quality.get().map(|q|q.1).unwrap_or_default()
                            data-tip=move ||quality.get().map(|q|q.1).unwrap_or_default()>{move ||quality.get().map(|q|q.0).unwrap_or_default()}</span>
                    </Show>
                    <super::screen::StreamWatchers ui=ui owner=owner kind=kind epoch=epoch/>
                    <Show when=move ||menu.get()==Some(key)>
                        <div class="media-menu-scrim" aria-hidden="true" on:pointerdown=move |e|e.stop_propagation() on:click=move |_|close_menu(true)></div>
                        <div class="media-item-menu message-options" class:up=menu_up class:start=menu_start role="menu" aria-label=label_menu.clone()
                            on:pointerdown=move |e|e.stop_propagation() on:click=move |e|e.stop_propagation()
                            on:keydown=move |e|super::keyboard::rove_items(&e,"[role=menuitem]")>
                            <p class="context-heading" aria-hidden="true">{label_heading.clone()}</p>
                            <Show when=several>
                                <button type="button" role="menuitem" class="context-item" on:click=move |_|{toggle_focus();close_menu(false);}>
                                    {move ||view!{<Icon kind=if on_stage.get() {Glyph::Grid} else {Glyph::Spotlight}/>}}
                                    {move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}}<kbd class="context-shortcut">"Enter"</kbd></button>
                            </Show>
                            <button type="button" role="menuitem" class="context-item" on:click=move |_|full()><Icon kind=Glyph::Expand/>{move ||crate::i18n::t("Full screen")}<kbd class="context-shortcut">"F"</kbd></button>
                            <Show when=several>
                                <button type="button" role="menuitem" class="context-item" disabled=first on:click=move |_|move_by(key,-1)><Icon kind=Glyph::ArrowLeft/>{move ||crate::i18n::t("Move earlier")}<kbd class="context-shortcut">"Alt+←"</kbd></button>
                                <button type="button" role="menuitem" class="context-item" disabled=last on:click=move |_|move_by(key,1)><Icon kind=Glyph::ArrowRight/>{move ||crate::i18n::t("Move later")}<kbd class="context-shortcut">"Alt+→"</kbd></button>
                            </Show>
                            <button type="button" role="menuitem" class="context-item" on:click=move |_|{
                                close_menu(false);exit_full();
                                hidden.update(|h|h.push(key));
                                announce(localized_format!(crate::i18n::language(), "Hidden: {}", tile_name(key)));
                                focus_selector(".media-bar-hidden .media-chip");
                            }><Icon kind=Glyph::EyeOff/>{move ||crate::i18n::t("Hide this stream")}</button>
                            {(!own).then(||view!{<div class="media-menu-volume"><super::context_menu::ParticipantVolume ui=ui guild=guild channel=channel member=owner/></div>})}
                        </div>
                    </Show>
                </figure>}
            }/>
            </div>
        </div>
    </div>}
}
