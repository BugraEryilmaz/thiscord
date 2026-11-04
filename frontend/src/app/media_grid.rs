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
    /// Content wider than the area scrolls sideways (compact phone strips).
    content_width: f64,
}

/// Frames counted at the last sample, for measuring the received frame rate.
type Samples = Rc<RefCell<HashMap<Key, (u32, f64)>>>;

const TIPS_KEY: &str = "thiscord.media-tips";
const TIP_USES: u32 = 3;

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}

static NEXT_GRID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn observe_size(element: &web_sys::Element, callback: impl FnMut() + 'static) {
    let callback = Closure::<dyn FnMut()>::new(callback);
    if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
        observer.observe(element);
        let resource = StoredValue::new_local((observer, callback));
        on_cleanup(move || resource.with_value(|(observer, _)| observer.disconnect()));
    }
}

fn reduced_motion() -> bool {
    let window = window();
    js_sys::Reflect::get(&window, &"matchMedia".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| {
            f.call1(&window, &"(prefers-reduced-motion: reduce)".into())
                .ok()
        })
        .and_then(|query| js_sys::Reflect::get(&query, &"matches".into()).ok())
        .and_then(|matches| matches.as_bool())
        .unwrap_or(false)
}

fn now() -> f64 {
    window().performance().map(|p| p.now()).unwrap_or_default()
}

fn focus_selector(selector: &str) -> bool {
    document()
        .query_selector(selector)
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        .is_some_and(|element| element.focus().is_ok())
}

#[component]
pub(super) fn ScreenViewer(
    ui: Ui,
    guild: GuildId,
    channel: ChannelId,
    /// A short strip above other content (private calls): on narrow screens
    /// its tiles keep full height in one row that scrolls sideways.
    #[prop(optional)]
    compact: bool,
) -> impl IntoView {
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
    let drag_origin = StoredValue::new(None::<(Vec<Key>, bool)>);
    // Until the user rearranges, tiles follow a shape-aware automatic order.
    let arranged = RwSignal::new(false);
    let menu_place = RwSignal::new((false, None::<f64>));
    let stage = NodeRef::<leptos::html::Div>::new();
    let announce = move |text: String| announcement.set(text);
    // The gesture hint retires after a few uses; it is remembered per device.
    let tips = RwSignal::new(
        storage()
            .and_then(|s| s.get_item(TIPS_KEY).ok().flatten())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0),
    );
    let used = move || {
        let next = tips.get_untracked().saturating_add(1);
        if next <= TIP_USES {
            tips.set(next);
            if let Some(s) = storage() {
                let _ = s.set_item(TIPS_KEY, &next.to_string());
            }
        }
    };

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
    // Stream shapes, rounded so resolution changes of the same shape don't
    // reorder anything.
    let shapes = Memo::new(move |_| {
        dims.with(|d| {
            d.iter()
                .filter(|(_, (w, h))| *w > 0 && *h > 0)
                .map(|(k, (w, h))| (*k, ((*w as f64 / *h as f64) * 100.0).round() as u32))
                .collect::<Vec<_>>()
        })
    });
    let aspect_of = move |shapes: &[(Key, u32)], key: &Key| {
        shapes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, a)| *a as f64 / 100.0)
            .unwrap_or(media_layout::DEFAULT_ASPECT)
    };
    // Until the user rearranges, the order is chosen to pack shapes well. It
    // settles once every stream has a shape (from the area at that moment, so
    // resizing never reshuffles); afterwards a stream that joins or is shown
    // again is placed where it fits best, and the others keep their order.
    let auto = RwSignal::new(Vec::<Key>::new());
    let settled = StoredValue::new(false);
    let placed = StoredValue::new(Vec::<Key>::new());
    let hidden_before = StoredValue::new(0usize);
    Effect::new(move |_| {
        if arranged.get() {
            return;
        }
        let keys: Vec<Key> = tiles.with(|t| t.iter().map(key_of).collect::<Vec<_>>());
        let keys: Vec<Key> = hidden.with(|h| keys.into_iter().filter(|k| !h.contains(k)).collect());
        let shapes_now = shapes.get();
        let known = |k: &Key| shapes_now.iter().any(|(s, _)| s == k);
        let hidden_now = hidden.with(Vec::len);
        if hidden_now > hidden_before.get_value() {
            settled.set_value(false);
        }
        hidden_before.set_value(hidden_now);
        let (width, height) = size.get_untracked();
        let gap = if width < 600.0 { 8.0 } else { 12.0 };
        let mut current = auto.get_untracked();
        current.retain(|k| keys.contains(k));
        let mut done = placed.get_value();
        done.retain(|k| keys.contains(k));
        if !settled.get_value() {
            if !keys.is_empty() && width > 0.0 && keys.iter().all(known) {
                let aspects: Vec<f64> = keys.iter().map(|k| aspect_of(&shapes_now, k)).collect();
                current = media_layout::arrange(width, height, gap, &aspects)
                    .into_iter()
                    .map(|i| keys[i])
                    .collect();
                done = keys.clone();
                settled.set_value(true);
            } else {
                current = keys.clone();
            }
        } else {
            for k in &keys {
                if !current.contains(k) {
                    current.push(*k);
                }
            }
            let newcomers: Vec<Key> = keys
                .iter()
                .filter(|k| known(k) && !done.contains(k))
                .copied()
                .collect();
            for k in &newcomers {
                if let Some(from) = current.iter().position(|c| c == k) {
                    let aspects: Vec<f64> =
                        current.iter().map(|c| aspect_of(&shapes_now, c)).collect();
                    let at = media_layout::insertion(width, height, gap, &aspects, from);
                    let item = current.remove(from);
                    current.insert(at.min(current.len()), item);
                }
                done.push(*k);
            }
        }
        placed.set_value(done);
        if current != auto.get_untracked() {
            auto.set(current);
        }
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
        keys.retain(|k| !hidden.contains(k));
        if !arranged.get() {
            let mut ordered: Vec<Key> =
                auto.with(|a| a.iter().copied().filter(|k| keys.contains(k)).collect());
            ordered.extend(
                keys.iter()
                    .filter(|k| !ordered.contains(k))
                    .copied()
                    .collect::<Vec<_>>(),
            );
            keys = ordered;
        }
        keys.into_iter()
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
        let aspects: Vec<f64> = shapes.with(|s| keys.iter().map(|k| aspect_of(s, k)).collect());
        let gap = if width < 600.0 { 8.0 } else { 12.0 };
        let focused = focus
            .get()
            .and_then(|f| keys.iter().position(|k| *k == f))
            .filter(|_| keys.len() > 1);
        let mut placement = match focused {
            Some(i) => media_layout::focus(width, height, gap, &aspects, i),
            None => media_layout::grid(width, height, gap, &aspects),
        };
        // A narrow call strip would shrink tiles below readable height.
        if compact
            && focused.is_none()
            && width < 600.0
            && keys.len() > 1
            && placement.tiles.iter().any(|t| t.h < 120.0)
        {
            placement = media_layout::row(height, gap, &aspects);
        }
        let content_width = placement
            .tiles
            .iter()
            .map(|t| t.x + t.w)
            .fold(width, f64::max);
        Arrangement {
            rects: keys.iter().copied().zip(placement.tiles).collect(),
            stage: focused.map(|i| keys[i]),
            height: placement.height,
            width,
            scrolls: placement.scrolls,
            content_width,
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
    let players: Rc<RefCell<HashMap<Key, web_sys::HtmlVideoElement>>> = Rc::default();
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
            let mut players = players.borrow_mut();
            players.retain(|k, v| keys.contains(k) && v.is_connected());
            for key in keys {
                let video = match players.get(&key) {
                    Some(video) => video.clone(),
                    None => {
                        let Some(video) = root
                            .query_selector(&format!("[data-media-key='{}'] video", key_text(key)))
                            .ok()
                            .flatten()
                            .and_then(|v| v.dyn_into::<web_sys::HtmlVideoElement>().ok())
                        else {
                            continue;
                        };
                        players.insert(key, video.clone());
                        video
                    }
                };
                let size = (video.video_width(), video.video_height());
                if size.0 > 0 && dims.with_untracked(|d| d.get(&key) != Some(&size)) {
                    dims.update(|d| {
                        d.insert(key, size);
                    });
                }
                let quality = video.get_video_playback_quality();
                let frames = quality
                    .total_video_frames()
                    .saturating_sub(quality.dropped_video_frames());
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
        if next.is_some() {
            used();
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
            if !arranged.get_untracked() {
                // The first move starts from what the user sees.
                let mut seeded = visible_keys.get_untracked();
                seeded.extend(
                    o.iter()
                        .filter(|k| !seeded.contains(k))
                        .copied()
                        .collect::<Vec<_>>(),
                );
                *o = seeded;
            }
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
        arranged.set(true);
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
    // Keyed rows move their DOM node, which drops focus; put it back on the
    // control that was used so repeated moves keep working.
    let refocus = move |key: Key, item: Option<&'static str>| {
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(0).await;
            let tile = format!("[data-media-key='{}']", key_text(key));
            let done = item.is_some_and(|item| {
                focus_selector(&format!(
                    "{tile} [data-move='{item}']:not([aria-disabled='true'])"
                )) || focus_selector(&format!("{tile} [data-move]:not([aria-disabled='true'])"))
            });
            if !done
                && let Some(content) = document()
                    .query_selector(&format!("{tile} .media-tile-content"))
                    .ok()
                    .flatten()
            {
                // Programmatic focus after a DOM move does not match
                // :focus-visible; mark it so the ring still shows.
                let _ = content.set_attribute("data-keyboard", "");
                focus_selector(&format!("{tile} .media-tile-content"));
            }
        });
    };
    // Moves `key` to `index` in the visible order.
    let move_index = move |key: Key, index: usize, item: Option<&'static str>| {
        let keys = visible_keys.get_untracked();
        let (Some(current), Some(target)) = (
            keys.iter().position(|k| *k == key),
            keys.get(index).copied(),
        ) else {
            return;
        };
        if current == index {
            return;
        }
        move_to(key, target);
        announce_position(key);
        refocus(key, item);
        used();
    };
    let move_by = move |key: Key, delta: isize, item: Option<&'static str>| {
        let current = visible_keys.with_untracked(|v| v.iter().position(|k| *k == key));
        if let Some(index) = current.and_then(|i| i.checked_add_signed(delta)) {
            move_index(key, index, item);
        }
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
            exit_full();
        } else if focus.with_untracked(Option::is_some)
            && document().active_element().is_none_or(|a| {
                a.tag_name() == "BODY"
                    || a.closest(".channel-streams").ok().flatten().is_some()
                        && !matches!(a.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
            })
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
    // Opening a menu keeps it inside the window, then moves keyboard focus to
    // its first item.
    Effect::new(move |_| {
        if let Some(key) = menu.get() {
            menu_place.set((false, None));
            leptos::task::spawn_local(async move {
                leptos::task::tick().await;
                let tile = format!("[data-media-key='{}']", key_text(key));
                let size = |v: Result<wasm_bindgen::JsValue, _>| {
                    v.ok().and_then(|v| v.as_f64()).unwrap_or(0.0)
                };
                let viewport = size(window().inner_height());
                if size(window().inner_width()) >= 768.0
                    && let (Some(figure), Some(popover)) = (
                        document().query_selector(&tile).ok().flatten(),
                        document()
                            .query_selector(&format!("{tile} .media-item-menu"))
                            .ok()
                            .flatten(),
                    )
                {
                    let (bounds, needed) = (
                        figure.get_bounding_client_rect(),
                        popover.scroll_height() as f64,
                    );
                    let below = viewport - (bounds.top() + 48.0) - 8.0;
                    let above = bounds.bottom() - 48.0 - 8.0;
                    let up = needed > below && above > below;
                    let room = if up { above } else { below };
                    menu_place.set((up, (needed > room).then_some(room.max(160.0))));
                }
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
                <span class="media-bar-hint" class:hidden=move ||{!several() || tips.get() >= TIP_USES}>{move ||if focused() {crate::i18n::t("Click a stream to put it on the stage · Esc returns to the grid")} else {crate::i18n::t("Click to focus · Drag to rearrange")}}</span>
                <Show when=move ||{several() && tips.get() >= TIP_USES}>
                    <button type="button" class="icon-button media-bar-help" aria-label=move ||crate::i18n::t("Grid gestures")
                        data-tip=move ||crate::i18n::t("Click a stream to focus it · Drag to rearrange · Right-click or ⋯ for options · On a focused stream: Enter, F, Alt+arrows")><Icon kind=Glyph::Keyboard/></button>
                </Show>
                <span class="sr-only" id=hint_id()>{move ||crate::i18n::t("Enter to focus, F for full screen, Alt and arrow keys to move, Shift+F10 for options.")}</span>
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
        <div node_ref=stage class="media-stage"
            on:dragover=move |e: web_sys::DragEvent|{if dragging.get_untracked().is_some(){e.prevent_default();}}
            on:drop=move |e: web_sys::DragEvent|e.prevent_default() class:empty=move ||visible.get().is_empty() class:scrolls=move ||arrangement.with(|a|a.scrolls) class:scrolls-x=move ||arrangement.with(|a|a.content_width>a.width+0.5) class:dragging=move ||dragging.get().is_some() class:focus-mode=focused>
            <div class="media-stage-content" style:height=move ||arrangement.with(|a| if a.scrolls {format!("{:.0}px", a.height)} else {"100%".into()})
                style:width=move ||arrangement.with(|a| if a.content_width>a.width+0.5 {format!("{:.0}px", a.content_width)} else {"100%".into()})>
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
                // A keyed row moved in the DOM loses its CSS transition and
                // would jump; animate it from where it was instead (FLIP).
                let shown_at=StoredValue::new(None::<Rect>);
                let fresh=RwSignal::new(true);
                Effect::new(move |_| {
                    let Some(next)=rect.get() else {return;};
                    let Some(previous)=shown_at.get_value() else {
                        shown_at.set_value(Some(next));
                        leptos::task::spawn_local(async move {
                            gloo_timers::future::TimeoutFuture::new(60).await;
                            fresh.try_set(false);
                        });
                        return;
                    };
                    shown_at.set_value(Some(next));
                    if expanded.get_untracked()==Some(key) || ((previous.x-next.x).abs()<1.0 && (previous.y-next.y).abs()<1.0) {return;}
                    leptos::task::spawn_local(async move {
                        gloo_timers::future::TimeoutFuture::new(0).await;
                        let Some(element)=figure.get_untracked() else {return;};
                        if reduced_motion() {return;}

                        let (left,top)=(element.offset_left() as f64,element.offset_top() as f64);
                        // Tiles still in place are animating on their own.
                        if (left-next.x).abs()>1.0 || (top-next.y).abs()>1.0 {return;}
                        let style=element.style();
                        let _=style.set_property("transition","none");
                        let _=style.set_property("transform",&format!("translate({:.1}px, {:.1}px) scale({:.4}, {:.4})",previous.x-next.x,previous.y-next.y,previous.w/next.w.max(1.0),previous.h/next.h.max(1.0)));
                        let _=element.offset_width();
                        let _=style.remove_property("transition");
                        let _=style.remove_property("transform");
                    });
                });
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
                let menu_up=move ||menu_place.get().0;
                let menu_start=move ||arrangement.with(|a|rect.get().is_some_and(|r|r.x+r.w<320.0 && a.width>r.x+r.w+8.0));
                let quality=Memo::new(move |_| {
                    let (w,h)=dims.with(|d|d.get(&key).copied())?;
                    let fps=rates.with(|r|r.get(&key).copied()).unwrap_or_default();
                    let resolution=media_layout::resolution_label(w,h);
                    let nominal=media_layout::nominal_fps(fps);
                    let language=crate::i18n::language();
                    let rate=(nominal>0).then(||localized_format!(language, "{} fps", nominal));
                    let compact=if nominal>0 {format!("{resolution}{nominal}")} else {resolution.clone()};
                    let detail=if own {localized_format!(language, "{}×{} · {} fps (your capture)", w, h, nominal)}
                        else {localized_format!(language, "{}×{} · {} fps received", w, h, nominal)};
                    Some((resolution,rate,detail,compact))
                });
                let hint=hint_id();
                let quality_id=format!("media-quality-{grid_id}-{}",key_text(key));
                let label_menu=label.clone();
                let label_heading=label.clone();
                view!{<figure node_ref=figure class="media-tile" data-media-key=key_text(key)
                    class:expanded=is_expanded class:stage=move ||on_stage.get() class:unplaced=move ||rect.get().is_none()
                    class:dragging=move ||dragging.get()==Some(key) class:drop-stage=move ||on_stage.get() && drop_stage.get()
                    class:menu-open=move ||menu.get()==Some(key) class:fresh=move ||fresh.get()
                    style:left=placed(|r|format!("{:.1}px",r.x)) style:top=placed(|r|format!("{:.1}px",r.y))
                    style:width=placed(|r|format!("{:.1}px",r.w)) style:height=placed(|r|format!("{:.1}px",r.h))
                    draggable=move ||if menu.get()==Some(key) || is_expanded() || on_stage.get() || !several() {"false"} else {"true"}
                    on:contextmenu=move |e|{e.prevent_default();e.stop_propagation();open_menu();}
                    on:dragstart=move |e: web_sys::DragEvent|{
                        if let Some(transfer)=e.data_transfer(){let _=transfer.set_data("text/plain",&name);transfer.set_effect_allowed("move");}
                        drag_origin.set_value(Some((order.get_untracked(),arranged.get_untracked())));
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
                    on:dragend=move |e: web_sys::DragEvent|{
                        let cancelled=e.data_transfer().is_some_and(|t|t.drop_effect()=="none");
                        if let Some((previous,was_arranged))=drag_origin.get_value() && cancelled {
                            order.set(previous);arranged.set(was_arranged);
                        }
                        drag_origin.set_value(None);
                        drop_stage.set(false);
                        if let Some(source)=dragging.get_untracked(){dragging.set(None);used();if !cancelled && focus.get_untracked()!=Some(source){announce_position(source);}}
                    }>
                    <div class="media-tile-content" role="button" tabindex="0" aria-label=label.clone() aria-describedby=format!("{hint} {quality_id}")
                        aria-pressed=move ||several().then(||on_stage.get().to_string())
                        on:click=move |_|toggle_focus()
                        on:blur=move |e|{if let Some(t)=e.target().and_then(|t|t.dyn_into::<web_sys::Element>().ok()){let _=t.remove_attribute("data-keyboard");}}
                        on:keydown=move |e|{
                            let key_name=e.key();
                            let plain=!e.alt_key()&&!e.ctrl_key()&&!e.meta_key();
                            match key_name.as_str() {
                                "Enter"|" " if plain && !e.shift_key() => {e.prevent_default();toggle_focus();}
                                "f"|"F" if plain => {e.prevent_default();full();}
                                "ArrowLeft"|"ArrowUp" if e.alt_key() => {e.prevent_default();move_by(key,-1,None);}
                                "ArrowRight"|"ArrowDown" if e.alt_key() => {e.prevent_default();move_by(key,1,None);}
                                "Home" if e.alt_key() => {e.prevent_default();move_index(key,0,None);}
                                "End" if e.alt_key() => {e.prevent_default();move_index(key,visible_keys.with_untracked(Vec::len).saturating_sub(1),None);}
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
                    <figcaption class="media-tile-caption"><Icon kind=if camera {Glyph::Camera} else {Glyph::Screen}/>
                        <crate::avatar::Avatar class="media-tile-avatar size-5 text-[10px]" name=member.display_name().to_owned() avatar_id=member.avatar_id/>
                        <span class="media-tile-name truncate">{member.display_name().to_owned()}</span></figcaption>
                    <div class="media-tile-actions">
                        <Show when=several>
                            <button type="button" class="media-tile-button media-tile-focus" tabindex="-1" aria-pressed=move ||on_stage.get().to_string()
                                aria-label=move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}
                                data-tip=move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}
                                on:click=move |_|toggle_focus()>{move ||view!{<Icon kind=if on_stage.get() {Glyph::Grid} else {Glyph::Spotlight}/>}}</button>
                        </Show>
                        <button type="button" class="media-tile-button media-tile-full" tabindex="-1"
                            aria-label=move ||if is_expanded() {crate::i18n::t("Exit full screen")} else {crate::i18n::t("Full screen")}
                            data-tip=move ||if is_expanded() {crate::i18n::t("Exit full screen")} else {crate::i18n::t("Full screen")}
                            on:click=move |_|full()>{move ||view!{<Icon kind=if is_expanded() {Glyph::Shrink} else {Glyph::Expand}/>}}</button>
                        <button type="button" class="media-tile-button media-tile-more" aria-haspopup="menu" aria-expanded=move ||(menu.get()==Some(key)).to_string()
                            aria-label=move ||crate::i18n::t("Stream options") data-tip=move ||crate::i18n::t("Stream options")
                            on:pointerdown=move |e|e.stop_propagation()
                            on:click=move |_|if menu.get_untracked()==Some(key) {menu.set(None);} else {open_menu();}><Icon kind=Glyph::More/></button>
                    </div>
                    <Show when=move ||quality.get().is_some()>
                        <span class="media-quality" id=quality_id.clone()>
                            <span class="media-quality-tip" aria-hidden="true">{move ||quality.get().map(|q|q.2).unwrap_or_default()}</span>
                            <span class="sr-only">{move ||quality.get().map(|q|q.2).unwrap_or_default()}</span>
                            <span class="media-quality-full" aria-hidden="true">{move ||quality.get().map(|q|q.0).unwrap_or_default()}
                                {move ||quality.get().and_then(|q|q.1).map(|rate|view!{" · "{rate}})}</span>
                            <span class="media-quality-compact" aria-hidden="true">{move ||quality.get().map(|q|q.3).unwrap_or_default()}</span>
                        </span>
                    </Show>
                    <super::screen::StreamWatchers ui=ui owner=owner kind=kind epoch=epoch/>
                    <Show when=move ||menu.get()==Some(key)>
                        <div class="media-menu-scrim" aria-hidden="true" on:pointerdown=move |e|e.stop_propagation() on:click=move |_|close_menu(true)></div>
                        <div class="media-item-menu message-options" class:up=menu_up class:start=menu_start
                            style:max-height=move ||menu_place.get().1.map(|h|format!("{h:.0}px")).unwrap_or_default()
                            on:pointerdown=move |e|e.stop_propagation() on:click=move |e|e.stop_propagation()>
                            <p class="context-heading" aria-hidden="true">{label_heading.clone()}
                                {move ||quality.get().map(|q|view!{<span class="context-heading-detail">{q.2}</span>})}</p>
                            <div class="media-menu-items" role="menu" aria-label=label_menu.clone() on:keydown=move |e|super::keyboard::rove_items(&e,"[role=menuitem]")>
                            <Show when=several>
                                <button type="button" role="menuitem" class="context-item" on:click=move |_|{toggle_focus();close_menu(false);}>
                                    {move ||view!{<Icon kind=if on_stage.get() {Glyph::Grid} else {Glyph::Spotlight}/>}}
                                    {move ||if on_stage.get() {crate::i18n::t("Back to grid")} else {crate::i18n::t("Focus")}}<kbd class="context-shortcut">"Enter"</kbd></button>
                            </Show>
                            <button type="button" role="menuitem" class="context-item" on:click=move |_|full()><Icon kind=Glyph::Expand/>{move ||crate::i18n::t("Full screen")}<kbd class="context-shortcut">"F"</kbd></button>
                            <Show when=several>
                                <button type="button" role="menuitem" class="context-item" data-move="start" aria-disabled=move ||first().to_string() on:click=move |_|if !first() {move_index(key,0,Some("start"))}><Icon kind=Glyph::First/>{move ||crate::i18n::t("Move to start")}<kbd class="context-shortcut">"Alt+Home"</kbd></button>
                                <button type="button" role="menuitem" class="context-item" data-move="earlier" aria-disabled=move ||first().to_string() on:click=move |_|if !first() {move_by(key,-1,Some("earlier"))}><Icon kind=Glyph::ArrowLeft/>{move ||crate::i18n::t("Move earlier")}<kbd class="context-shortcut">"Alt+←"</kbd></button>
                                <button type="button" role="menuitem" class="context-item" data-move="later" aria-disabled=move ||last().to_string() on:click=move |_|if !last() {move_by(key,1,Some("later"))}><Icon kind=Glyph::ArrowRight/>{move ||crate::i18n::t("Move later")}<kbd class="context-shortcut">"Alt+→"</kbd></button>
                                <button type="button" role="menuitem" class="context-item" data-move="end" aria-disabled=move ||last().to_string() on:click=move |_|if !last() {move_index(key,visible_keys.with_untracked(Vec::len).saturating_sub(1),Some("end"))}><Icon kind=Glyph::Last/>{move ||crate::i18n::t("Move to end")}<kbd class="context-shortcut">"Alt+End"</kbd></button>
                            </Show>
                            <button type="button" role="menuitem" class="context-item" on:click=move |_|{
                                close_menu(false);exit_full();
                                hidden.update(|h|h.push(key));
                                announce(localized_format!(crate::i18n::language(), "Hidden: {}", tile_name(key)));
                                focus_selector(".media-bar-hidden .media-chip");
                            }><Icon kind=Glyph::EyeOff/>{move ||crate::i18n::t("Hide this stream")}</button>
                            </div>
                            {(!own).then(||view!{<div class="media-menu-volume" role="group" aria-label=move ||crate::i18n::t("Volume")><super::context_menu::ParticipantVolume ui=ui guild=guild channel=channel member=owner/></div>})}
                        </div>
                    </Show>
                </figure>}
            }/>
            </div>
        </div>
    </div>}
}
