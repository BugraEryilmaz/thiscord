use leptos::prelude::*;
use wasm_bindgen::JsCast;

fn trigger(target: Option<web_sys::EventTarget>) -> Option<web_sys::Element> {
    target?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest("[data-tip]")
        .ok()?
}

/// One root-level tooltip keeps hints above the rail and outside scroll clipping.
#[component]
pub(super) fn Host() -> impl IntoView {
    let active = StoredValue::new_local(None::<web_sys::Element>);
    let text = RwSignal::new(String::new());
    let position = RwSignal::new((0.0, 0.0));
    let visible = RwSignal::new(false);
    let tooltip = NodeRef::<leptos::html::Div>::new();
    let clear = move || {
        if let Some(element) = active.get_value() {
            let _ = element.remove_attribute("aria-describedby");
        }
        active.set_value(None);
        visible.set(false);
    };
    let show = move |element: Option<web_sys::Element>| {
        let Some(element) = element else {
            clear();
            return;
        };
        let Some(label) = element.get_attribute("data-tip") else {
            clear();
            return;
        };
        if active.get_value().as_ref() != Some(&element) {
            clear();
        }
        let _ = element.set_attribute("aria-describedby", "thiscord-tooltip");
        active.set_value(Some(element));
        text.set(label);
    };
    Effect::new(move |_| {
        text.track();
        let Some(node) = tooltip.get() else {
            return;
        };
        let Some(element) = active.get_value() else {
            return;
        };
        let rect = element.get_bounding_client_rect();
        let size = node.get_bounding_client_rect();
        let width = window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(800.0);
        let height = window()
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(600.0);
        let right = element.closest(".server-rail").ok().flatten().is_some();
        let (x, y) = if right {
            (
                rect.right() + 12.0,
                rect.top() + (rect.height() - size.height()) / 2.0,
            )
        } else {
            (
                rect.left() + (rect.width() - size.width()) / 2.0,
                if rect.top() >= size.height() + 16.0 {
                    rect.top() - size.height() - 8.0
                } else {
                    rect.bottom() + 8.0
                },
            )
        };
        position.set((
            x.clamp(8.0, (width - size.width() - 8.0).max(8.0)),
            y.clamp(8.0, (height - size.height() - 8.0).max(8.0)),
        ));
        visible.set(true);
    });
    let over = window_event_listener(leptos::ev::mouseover, move |e| show(trigger(e.target())));
    let focus = window_event_listener(leptos::ev::focusin, move |e| show(trigger(e.target())));
    let out = window_event_listener(leptos::ev::mouseout, move |e| {
        if trigger(e.target()) != trigger(e.related_target()) {
            clear();
        }
    });
    let blur = window_event_listener(leptos::ev::focusout, move |_| clear());
    let click = window_event_listener(leptos::ev::click, move |_| clear());
    let context = window_event_listener(leptos::ev::contextmenu, move |_| clear());
    let key = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            clear();
        }
    });
    let resize = window_event_listener(leptos::ev::resize, move |_| clear());
    let wheel = window_event_listener(leptos::ev::wheel, move |_| clear());
    on_cleanup(move || {
        over.remove();
        focus.remove();
        out.remove();
        blur.remove();
        click.remove();
        context.remove();
        key.remove();
        resize.remove();
        wheel.remove();
    });
    view! { <div node_ref=tooltip id="thiscord-tooltip" role="tooltip" class="quick-tooltip"
    style:left=move ||format!("{}px",position.get().0) style:top=move ||format!("{}px",position.get().1)
    style:visibility=move ||if visible.get(){"visible"}else{"hidden"}>{move ||text.get()}</div> }
}
