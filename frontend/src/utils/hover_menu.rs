use leptos::{logging::log, prelude::*};
use stylance::classes;
use web_sys::HtmlDivElement;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverMenuDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverMenuTrigger {
    Click,
    Hover,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverMenuBackgroundStyle {
    Brightness,
    Blur,
}

struct BoundingClientRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    width: f64,
    height: f64,
}

impl From<web_sys::DomRect> for BoundingClientRect {
    fn from(rect: web_sys::DomRect) -> Self {
        BoundingClientRect {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
            width: rect.width(),
            height: rect.height(),
        }
    }
}

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "hover_menu.css"
);

#[component]
pub fn HoverMenu<I: IntoView, P: IntoView>(
    item: I,
    popup: P,
    direction: HoverMenuDirection,
    trigger: HoverMenuTrigger,
    #[prop(optional)]
    background_style: Vec<HoverMenuBackgroundStyle>,
    #[prop(default = RwSignal::new(false))]
    visible: RwSignal<bool>,
) -> impl IntoView {
    let parent_ref = NodeRef::new();
    let popup_ref = NodeRef::new();
    let (x, set_x) = signal(0);
    let (y, set_y) = signal(0);
    let (_modified, set_modified) = signal(true);

    let calc = move || {
        if let Some(parent) = parent_ref.get() {
            if let Some(popup) = popup_ref.get() {
                let parent: HtmlDivElement = parent;
                let popup: HtmlDivElement = popup;
                let parent_rect: BoundingClientRect = parent.get_bounding_client_rect().into();
                let mut popup_rect: BoundingClientRect = popup.get_bounding_client_rect().into();
                popup_rect.width += 32.0; // Adjust for padding and margin 2rem
                popup_rect.height += 32.0; // Adjust for padding and margin 2rem
                match direction {
                    HoverMenuDirection::Up => {
                        set_x.set(
                            (parent_rect.left + parent_rect.width / 2.0 - popup_rect.width / 2.0)
                                as i32,
                        );
                        set_y.set((parent_rect.top - popup_rect.height) as i32);
                    }
                    HoverMenuDirection::Down => {
                        set_x.set(
                            (parent_rect.left + parent_rect.width / 2.0 - popup_rect.width / 2.0)
                                as i32,
                        );
                        set_y.set((parent_rect.bottom) as i32);
                    }
                    HoverMenuDirection::Left => {
                        set_x.set((parent_rect.left - popup_rect.width) as i32);
                        set_y.set(
                            (parent_rect.top + parent_rect.height / 2.0 - popup_rect.height / 2.0)
                                as i32,
                        );
                    }
                    HoverMenuDirection::Right => {
                        set_x.set((parent_rect.right) as i32);
                        set_y.set(
                            (parent_rect.top + parent_rect.height / 2.0 - popup_rect.height / 2.0)
                                as i32,
                        );
                    }
                }
            }
        }
    };

    view! {
        <div>
            <div 
                class=style::hover_menu_item
                node_ref=parent_ref
                on:mouseover=move |_| {
                    if matches!(trigger.clone(), HoverMenuTrigger::Hover) {
                        calc();
                        log!("HoverMenu: Mouse entered, calculating position and showing popup");
                        set_modified.set(false);
                    }
                }
                on:scroll=move |_| {
                    if matches!(trigger.clone(), HoverMenuTrigger::Hover) {
                        set_modified.set(true);
                    }
                }
                on:click=move |_| {
                    if matches!(trigger.clone(), HoverMenuTrigger::Click) {
                        calc();
                        visible.set(true);
                    }
                }
            >
                {item}
            </div>
            <div node_ref=popup_ref
                style:left=move || format!("{}px", x.get())
                style:top=move || format!("{}px", y.get())
                class=move || {classes!(
                    if trigger == HoverMenuTrigger::Click && visible.get() { style::hover_menu_popup_visible }
                    else if trigger == HoverMenuTrigger::Click { style::hover_menu_popup_hidden }
                    else { style::hover_menu_popup_whenhover },
                    style::hover_menu_popup,
                )}
            >
                {popup}
            </div>
            <div on:click=move |_| {
                    visible.set(false);
                }
                class=move || {classes!(
                    if trigger == HoverMenuTrigger::Click && visible.get() { style::hover_menu_popup_visible }
                    else if trigger == HoverMenuTrigger::Click { style::hover_menu_popup_hidden }
                    else { style::hover_menu_popup_whenhover },
                    if background_style.contains(&HoverMenuBackgroundStyle::Blur) {
                        Some(style::hover_menu_popup_background_blur)
                    } else {
                        None
                    },
                    if background_style.contains(&HoverMenuBackgroundStyle::Brightness) {
                        Some(style::hover_menu_popup_background_brightness)
                    } else {
                        None
                    },
                    style::hover_menu_popup_background
                )}
            >
                // This div is used to close the popup when clicking outside
            </div>
        </div>
    }
}
