//! Unicode emoji palette for composers and reactions. Grids render only while open.
use super::icons::{Glyph, Icon};
use leptos::prelude::*;
use thiscord_shared::chat::EMOJI_GROUPS;
use wasm_bindgen::JsCast;

/// Every palette emoji, grouped under translated headings.
#[component]
pub(super) fn EmojiGrid(pick: Callback<&'static str>) -> impl IntoView {
    view! {
        <div class="emoji-grid">
            {EMOJI_GROUPS.iter().map(|&(label, group)| view! {
                <section role="group" aria-label=move || crate::i18n::t(label)>
                    <h4>{move || crate::i18n::t(label)}</h4>
                    <div class="emoji-grid-items">
                        {group.iter().map(|&emoji| view! {
                            <button type="button" aria-label=emoji title=emoji on:click=move |_| pick.run(emoji)>{emoji}</button>
                        }).collect_view()}
                    </div>
                </section>
            }).collect_view()}
        </div>
    }
}

/// Inserts `text` at the textarea's current selection and restores focus after it.
pub(super) fn insert_at_caret(
    input: NodeRef<leptos::html::Textarea>,
    draft: RwSignal<String>,
    text: &str,
) {
    let element = input.get_untracked();
    let current = draft.get_untracked();
    let end = current.encode_utf16().count() as u32;
    let (start, stop) = element.as_ref().map_or((end, end), |e| {
        (
            e.selection_start().ok().flatten().unwrap_or(end),
            e.selection_end().ok().flatten().unwrap_or(end),
        )
    });
    let Some((next, caret)) = thiscord_frontend::mentions::splice(&current, start, stop, text)
    else {
        return;
    };
    draft.set(next);
    if let Some(element) = element {
        leptos::task::spawn_local(async move {
            // Let the reactive value reach the element before moving its caret.
            leptos::task::tick().await;
            let _ = element.focus();
            let _ = element.set_selection_range(caret, caret);
        });
    }
}

/// Composer button opening the palette above itself; picks insert at the caret.
#[component]
pub(super) fn ComposerEmoji(
    input: NodeRef<leptos::html::Textarea>,
    draft: RwSignal<String>,
    #[prop(into)] disabled: Signal<bool>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let root = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
            let inside = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|target| {
                    root.get_untracked()
                        .is_some_and(|root| root.contains(Some(&target)))
                });
            if !inside {
                open.set(false);
            }
        });
        on_cleanup(move || outside.remove());
    });
    Effect::new(move |_| {
        if disabled.get() {
            open.set(false);
        }
    });
    let pick = Callback::new(move |emoji: &'static str| insert_at_caret(input, draft, emoji));
    view! {
        <div node_ref=root class="relative"
            on:keydown=move |e| {
                if e.key() == "Escape" && open.get_untracked() {
                    e.prevent_default();
                    e.stop_propagation();
                    open.set(false);
                    if let Some(input) = input.get_untracked() { let _ = input.focus(); }
                }
            }>
            <button type="button" class="icon-button" aria-label=move || crate::i18n::t("Emoji") data-tip=move || crate::i18n::t("Emoji")
                aria-haspopup="dialog" aria-expanded=move || open.get().to_string() disabled=move || disabled.get()
                on:click=move |_| open.update(|value| *value = !*value)><Icon kind=Glyph::Reaction/></button>
            <Show when=move || open.get()>
                <div class="emoji-popover" role="dialog" aria-label=move || crate::i18n::t("Emoji")>
                    <EmojiGrid pick/>
                </div>
            </Show>
        </div>
    }
}
