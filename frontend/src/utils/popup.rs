use leptos::prelude::*;
use stylance::classes;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PopupBackgroundStyle {
    Brightness,
    Blur,
}

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "popup.css"
);

#[component]
pub fn Popup(
    children: ChildrenFn, // Accept children as a closure
    #[prop(optional)] background_style: Vec<PopupBackgroundStyle>,
    visible: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <Show when=move || visible.get()>
            <div
                class=classes!(
                    style::overlay,
                    if background_style.contains(&PopupBackgroundStyle::Blur) {
                        Some(style::popup_background_blur)
                    } else {
                        None
                    }
                )
                on:click=move |_| visible.set(false)
            >
                <div
                    class=classes!(
                        style::overlay,
                if background_style.contains(&PopupBackgroundStyle::Brightness) {
                    Some(style::popup_background_brightness)
                } else {
                    None
                }
                    )
                    on:click=move |_| visible.set(false)
                ></div>
            </div>
            <div class=style::popup_content>
                // Call the children closure
                {children()}
            </div>
        </Show>
    }
}
