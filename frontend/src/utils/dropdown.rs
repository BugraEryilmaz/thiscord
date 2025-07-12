use leptos::prelude::*;

use crate::utils::hover_menu::{HoverMenu, HoverMenuDirection, HoverMenuTrigger};

stylance::import_style!(
    #[allow(dead_code)]
    style,
    "dropdown.css"
);


#[component]
pub fn Dropdown(
    item: impl Fn() -> String + Send + Sync + 'static,
    drop_list: impl Fn() -> Vec<String> + Send + Sync + 'static,
    #[prop(optional, default = None)]
    callback: Option<impl Fn(String) + Send + Sync + Clone + 'static>,
) -> impl IntoView {
    let popup_visible = RwSignal::new(false);
    view! {
        <HoverMenu
            item=move || {
                view! {
                    <p class=style::current_item>
                        {item()}
                    </p>
                }
            }
            popup={
                view! {
                    <div class=style::popup>
                        <For
                            each=move || drop_list()
                            key=|element| element.clone()
                            let(element)
                        >
                            <p
                                class=style::option
                                on:click={
                                    let callback = callback.clone();
                                    move |_| {
                                        popup_visible.set(false);
                                        let e = element.clone();
                                        callback.as_ref().map(|f| f(e));
                                    }
                                }
                            >
                                {element.clone()}
                            </p>
                        </For>
                    </div>
                }
            }
            direction=HoverMenuDirection::Down
            trigger=HoverMenuTrigger::Click
            visible=popup_visible
        />
    }
}
