//! Device-local theme and accent color settings. Changes apply immediately;
//! the stored state lives in `crate::theme`.
use crate::i18n::t;
use crate::theme;
use leptos::prelude::*;
use thiscord_frontend::appearance::{ACCENTS, Appearance, DEFAULT_ACCENT, Rgb, Theme};

#[component]
pub(super) fn Panel() -> impl IntoView {
    let failed = RwSignal::new(false);
    let save = move |appearance: Appearance| failed.set(!theme::save(appearance));
    let accent = |color: Rgb| (color != DEFAULT_ACCENT).then_some(color);
    let custom = move || {
        let current = theme::current().accent();
        !ACCENTS.iter().any(|(_, color)| *color == current)
    };
    let picked = move |e: &leptos::ev::Event| Rgb::parse(&event_target_value(e));
    view! {
        <section class="settings-card">
            <h3>{move || crate::i18n::t("Theme")}</h3>
            <p class="settings-hint">{move || crate::i18n::t("Backgrounds and panels across the app.")}</p>
            <div class="appearance-themes" role="radiogroup" aria-label=move || crate::i18n::t("Theme")>
                {Theme::ALL.into_iter().map(|choice| {
                    let palette = choice.palette();
                    view! {
                        <button type="button" role="radio" class="appearance-theme"
                            aria-checked=move || (theme::current().theme == choice).to_string()
                            on:click=move |_| save(Appearance { theme: choice, ..theme::current_untracked() })>
                            <span class="appearance-theme-swatch" aria-hidden="true">
                                <i style:background-color=palette.rail.hex()></i>
                                <i style:background-color=palette.sidebar.hex()></i>
                                <i style:background-color=palette.surface.hex()><b style:background-color=move || theme::current().accent().hex()></b></i>
                            </span>
                            <strong>{move || t(choice.label())}</strong>
                            <small>{move || t(choice.description())}</small>
                        </button>
                    }
                }).collect_view()}
            </div>
        </section>
        <section class="settings-card">
            <h3>{move || crate::i18n::t("Accent color")}</h3>
            <p class="settings-hint">{move || crate::i18n::t("Used for buttons, highlights, selections and focus outlines.")}</p>
            <div class="appearance-accents" role="radiogroup" aria-label=move || crate::i18n::t("Accent color")>
                {ACCENTS.map(|(label, color)| view! {
                    <button type="button" role="radio" class="appearance-accent"
                        style:background-color=color.hex()
                        aria-label=move || t(label) data-tip=move || t(label)
                        aria-checked=move || (theme::current().accent() == color).to_string()
                        on:click=move |_| save(Appearance { accent: accent(color), ..theme::current_untracked() })></button>
                })}
                <label class="appearance-custom" class:selected=custom data-tip=move || crate::i18n::t("Custom color")>
                    <input type="color" aria-label=move || crate::i18n::t("Custom color")
                        prop:value=move || theme::current().accent().hex()
                        on:input=move |e| if let Some(color) = picked(&e) {
                            theme::preview(Appearance { accent: accent(color), ..theme::current_untracked() });
                        }
                        on:change=move |e| if let Some(color) = picked(&e) {
                            save(Appearance { accent: accent(color), ..theme::current_untracked() });
                        }/>
                    <span>{move || crate::i18n::t("Custom")}</span>
                </label>
            </div>
            <Show when=move || theme::current().low_contrast()>
                <p role="status" class="text-sm text-amber-200">{move || crate::i18n::t("This color may be hard to read on the selected theme.")}</p>
            </Show>
        </section>
        <section class="settings-card">
            <h3>{move || crate::i18n::t("Preview")}</h3>
            <div class="appearance-preview" aria-hidden="true">
                <div class="appearance-preview-rail"><span class="selected"></span><span></span><span></span></div>
                <div class="appearance-preview-sidebar"><span class="selected">{move || format!("# {}", crate::i18n::t("general"))}</span><span>{move || format!("# {}", crate::i18n::t("random"))}</span></div>
                <div class="appearance-preview-chat">
                    <p><strong>"Thiscord"</strong><span class="appearance-preview-mention">"@you"</span>{move || crate::i18n::t("This is how your messages and mentions will look.")}</p>
                    <div class="appearance-preview-actions"><span class="bg-brand">{move || crate::i18n::t("Button")}</span><span class="appearance-preview-link">{move || crate::i18n::t("Link")}</span></div>
                </div>
            </div>
        </section>
        <div class="settings-actions">
            <button type="button" class="settings-secondary" disabled=move || theme::current() == Appearance::default()
                on:click=move |_| save(Appearance::default())>{move || crate::i18n::t("Reset to default")}</button>
        </div>
        <Show when=move || failed.get()>
            <p role="status" class="text-sm text-amber-200">{move || crate::i18n::t("Your appearance preference could not be saved. It will apply until you close this window.")}</p>
        </Show>
    }
}
