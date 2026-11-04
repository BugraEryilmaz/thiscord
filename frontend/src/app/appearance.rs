//! Device-local theme and color settings. Changes apply immediately; the
//! stored state lives in `crate::theme`.
use crate::i18n::t;
use crate::theme;
use leptos::prelude::*;
use thiscord_frontend::appearance::{ACCENTS, Appearance, Group, Rgb, Theme, Token};

/// Accepts `#rrggbb` or `rrggbb` typed into a hex field.
fn typed_color(value: &str) -> Option<Rgb> {
    let value = value.trim();
    Rgb::parse(value).or_else(|| Rgb::parse(&format!("#{value}")))
}

fn set_input(e: &leptos::ev::Event, value: &str) {
    event_target::<web_sys::HtmlInputElement>(e).set_value(value);
}

#[component]
pub(super) fn Panel() -> impl IntoView {
    let failed = RwSignal::new(false);
    let save = move |appearance: Appearance| failed.set(!theme::save(appearance));
    let set_color = move |token: Token, color: Option<Rgb>| {
        save(theme::current_untracked().with(token, color));
    };
    let custom_accent = move || {
        let current = theme::current().accent();
        !ACCENTS.iter().any(|(_, color)| *color == current)
    };
    let picked = move |e: &leptos::ev::Event| Rgb::parse(&event_target_value(e));
    view! {
        <section class="settings-card">
            <h3>{move || t("Theme")}</h3>
            <p class="settings-hint">{move || t("A starting point for backgrounds and text. Choosing one replaces your custom background and text colors.")}</p>
            <div class="appearance-themes" role="radiogroup" aria-label=move || t("Theme") on:keydown=move |e| super::keyboard::rove_radio(&e)>
                {Theme::ALL.into_iter().map(|choice| {
                    let palette = choice.palette();
                    view! {
                        <button type="button" role="radio" class="appearance-theme"
                            aria-checked=move || (theme::current().theme == choice).to_string()
                            tabindex=move || if theme::current().theme == choice { "0" } else { "-1" }
                            on:click=move |_| save(theme::current_untracked().with_theme(choice))>
                            <span class="appearance-theme-swatch" aria-hidden="true">
                                <i style:background-color=palette.get(Token::Rail).hex()></i>
                                <i style:background-color=palette.get(Token::Sidebar).hex()></i>
                                <i style:background-color=palette.get(Token::Surface).hex()>
                                    <em style:background-color=palette.get(Token::Text).hex()></em>
                                    <b style:background-color=move || theme::current().accent().hex()></b>
                                </i>
                            </span>
                            <strong>{move || t(choice.label())}</strong>
                            <small>{move || t(choice.description())}</small>
                        </button>
                    }
                }).collect_view()}
            </div>
        </section>
        <section class="settings-card">
            <h3>{move || t("Accent color")}</h3>
            <p class="settings-hint">{move || t("Used for buttons, highlights, selections and focus outlines.")}</p>
            <div class="appearance-accents" role="radiogroup" aria-label=move || t("Accent color") on:keydown=move |e| super::keyboard::rove_radio(&e)>
                {ACCENTS.map(|(label, color)| view! {
                    <button type="button" role="radio" class="appearance-accent"
                        tabindex=move || if theme::current().accent() == color || (custom_accent() && color == ACCENTS[0].1) { "0" } else { "-1" }
                        style:background-color=color.hex()
                        aria-label=move || t(label) data-tip=move || t(label)
                        aria-checked=move || (theme::current().accent() == color).to_string()
                        on:click=move |_| set_color(Token::Accent, Some(color))></button>
                })}
                <label class="appearance-custom" class:selected=custom_accent data-tip=move || t("Custom color")>
                    <input type="color" aria-label=move || t("Custom color")
                        prop:value=move || theme::current().accent().hex()
                        on:input=move |e| if let Some(color) = picked(&e) {
                            theme::preview(theme::current_untracked().with(Token::Accent, Some(color)));
                        }
                        on:change=move |e| if let Some(color) = picked(&e) {
                            set_color(Token::Accent, Some(color));
                        }/>
                    <span>{move || t("Custom")}</span>
                </label>
            </div>
        </section>
        <section class="settings-card">
            <h3>{move || t("Colors")}</h3>
            <p class="settings-hint">{move || t("Fine-tune every color. Type a hex code or pick a color; the reset button returns a color to the theme's default.")}</p>
            {Group::ALL.map(|group| view! {
                <div class="appearance-color-group" role="group" aria-label=move || t(group.label())>
                    <h4>{move || t(group.label())}</h4>
                    {group.tokens().map(|token| view! { <ColorRow token set_color/> }).collect_view()}
                </div>
            })}
        </section>
        <Show when=move || !theme::current().hard_to_read().is_empty()>
            <section class="settings-card appearance-warnings" role="status">
                <h3>{move || t("Hard to read")}</h3>
                <p class="settings-hint">{move || t("These colors have low contrast together. Some text may be difficult to read.")}</p>
                <ul>
                    {move || theme::current().hard_to_read().into_iter().map(|(fg, bg)| view! {
                        <li><span style:background-color=theme::current_untracked().color(fg).hex()></span>{t(fg.label())}" · "<span style:background-color=theme::current_untracked().color(bg).hex()></span>{t(bg.label())}</li>
                    }).collect_view()}
                </ul>
            </section>
        </Show>
        <section class="settings-card">
            <h3>{move || t("Preview")}</h3>
            <div class="appearance-preview" aria-hidden="true">
                <div class="appearance-preview-rail"><span class="selected"></span><span></span><span></span></div>
                <div class="appearance-preview-sidebar">
                    <span class="selected">{move || format!("# {}", t("general"))}</span>
                    <span>{move || format!("# {}", t("random"))}</span>
                    <span class="appearance-preview-voice"><i></i>"Thiscord"</span>
                </div>
                <div class="appearance-preview-chat">
                    <p><strong>"Thiscord"</strong><time>"12:30"</time></p>
                    <p><span class="appearance-preview-mention">"@you"</span>{move || t("This is how your messages and mentions will look.")}" "<span class="appearance-preview-link">"thiscord.com.tr"</span></p>
                    <div class="appearance-preview-actions">
                        <span class="bg-brand">{move || t("Button")}</span>
                        <span class="appearance-preview-online">{move || t("Online")}</span>
                        <span class="appearance-preview-warning">{move || t("Warning")}</span>
                        <span class="appearance-preview-danger">{move || t("Danger")}</span>
                    </div>
                </div>
                <div class="appearance-preview-dock"><i></i><span>{move || t("Account panel")}</span></div>
                <div class="appearance-preview-menu">
                    <span>{move || t("Menus")}</span>
                    <span class="selected">{move || t("Accent")}</span>
                </div>
            </div>
        </section>
        <ShareCard save/>
        <div class="settings-actions">
            <button type="button" class="settings-secondary" disabled=move || theme::current() == Appearance::default()
                on:click=move |_| save(Appearance::default())>{move || t("Reset to default")}</button>
        </div>
        <Show when=move || failed.get()>
            <p role="status" class="text-sm text-amber-200">{move || t("Your appearance preference could not be saved. It will apply until you close this window.")}</p>
        </Show>
    }
}

#[component]
fn ColorRow(
    token: Token,
    set_color: impl Fn(Token, Option<Rgb>) + Copy + 'static,
) -> impl IntoView {
    let hex = move || theme::current().color(token).hex();
    let label = move || t(token.label());
    view! {
        <div class="appearance-color-row" class:custom=move || theme::current().is_custom(token)>
            <label class="appearance-color-swatch">
                <input type="color" aria-label=label prop:value=hex
                    on:input=move |e| if let Some(color) = Rgb::parse(&event_target_value(&e)) {
                        theme::preview(theme::current_untracked().with(token, Some(color)));
                    }
                    on:change=move |e| if let Some(color) = Rgb::parse(&event_target_value(&e)) {
                        set_color(token, Some(color));
                    }/>
            </label>
            <div class="appearance-color-label">
                <strong>{label}</strong>
                <small>{move || t(token.description())}</small>
            </div>
            <input type="text" class="appearance-hex" spellcheck="false" autocomplete="off" maxlength="7"
                aria-label=move || format!("{} ({})", label(), t("hex code"))
                prop:value=hex
                on:change=move |e| match typed_color(&event_target_value(&e)) {
                    Some(color) => {
                        set_color(token, Some(color));
                        set_input(&e, &theme::current_untracked().color(token).hex());
                    }
                    None => set_input(&e, &theme::current_untracked().color(token).hex()),
                }/>
            <button type="button" class="appearance-reset"
                aria-label=move || format!("{}: {}", label(), t("Use theme color"))
                data-tip=move || t("Use theme color")
                disabled=move || !theme::current().is_custom(token)
                on:click=move |_| set_color(token, None)>
                <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M3 12a9 9 0 1 0 3-6.7"/><path d="M3 4v5h5"/>
                </svg>
            </button>
        </div>
    }
}

#[component]
fn ShareCard(save: impl Fn(Appearance) + Copy + 'static) -> impl IntoView {
    let copied = RwSignal::new(false);
    let import = RwSignal::new(String::new());
    let invalid = RwSignal::new(false);
    let code = move || theme::current().encode();
    let copy = move |_| {
        let promise = window().navigator().clipboard().write_text(&code());
        leptos::task::spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                copied.set(true);
                gloo_timers::future::TimeoutFuture::new(2_000).await;
                copied.set(false);
            }
        });
    };
    let apply = move || match Appearance::decode(&import.get_untracked()) {
        Some(appearance) => {
            save(appearance);
            import.set(String::new());
            invalid.set(false);
        }
        None => invalid.set(true),
    };
    view! {
        <section class="settings-card">
            <h3>{move || t("Share theme")}</h3>
            <p class="settings-hint">{move || t("Copy this code to use the same colors on another device, or paste a code someone shared with you.")}</p>
            <div class="appearance-code">
                <input type="text" readonly aria-label=move || t("Theme code") prop:value=code
                    on:focus=move |e| { event_target::<web_sys::HtmlInputElement>(&e).select() }/>
                <button type="button" class="settings-secondary" on:click=copy>
                    {move || if copied.get() { t("Copied") } else { t("Copy") }}
                </button>
            </div>
            <form class="appearance-code" on:submit=move |e| { e.prevent_default(); apply(); }>
                <input type="text" spellcheck="false" autocomplete="off" maxlength="512"
                    aria-label=move || t("Paste a theme code") placeholder=move || t("Paste a theme code")
                    aria-invalid=move || invalid.get().to_string()
                    prop:value=move || import.get()
                    on:input=move |e| { import.set(event_target_value(&e)); invalid.set(false); }/>
                <button type="submit" class="settings-secondary" disabled=move || import.get().trim().is_empty()>{move || t("Apply")}</button>
            </form>
            <Show when=move || invalid.get()>
                <p role="alert" class="text-sm text-red-300">{move || t("That theme code isn't valid.")}</p>
            </Show>
        </section>
    }
}
