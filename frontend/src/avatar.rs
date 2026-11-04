use leptos::prelude::*;
use thiscord_shared::{AvatarId, account::AVATAR_PATH};

/// Initials remain visible if an image is unavailable or fails to load.
#[component]
pub fn Avatar(
    #[prop(into)] name: Signal<String>,
    #[prop(into)] avatar_id: Signal<Option<AvatarId>>,
    #[prop(default = "size-10")] class: &'static str,
) -> impl IntoView {
    let failed = RwSignal::new(None::<AvatarId>);
    let source = Memo::new(move |_| avatar_id.get().filter(|id| failed.get() != Some(*id)));
    view! {
        <span class=format!("relative inline-grid shrink-0 place-items-center overflow-hidden rounded-full bg-white/10 font-semibold {class}") aria-hidden="true">
            {move || name.get().chars().next().unwrap_or('?').to_uppercase().to_string()}
            {move || source.get().map(|id| {
                let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
                view! {
                    <img class="absolute inset-0 size-full object-cover" alt="" draggable="false"
                        referrerpolicy="no-referrer" src=format!("{}{AVATAR_PATH}/{id}", base.trim_end_matches('/'))
                        on:error=move |_| failed.set(Some(id))/>
                }
            })}
        </span>
    }
}

/// Up to two initials, as shown in the server rail when a guild has no icon.
pub fn guild_initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|s| s.chars().next())
        .collect::<String>()
        .to_uppercase()
}

/// Guild initials with the guild icon layered on top. Place inside a positioned,
/// clipped container (`relative overflow-hidden`); initials stay visible if the
/// image is unavailable or fails to load.
#[component]
pub fn GuildIcon(
    #[prop(into)] name: Signal<String>,
    #[prop(into)] icon_id: Signal<Option<thiscord_shared::GuildIconId>>,
) -> impl IntoView {
    use thiscord_shared::permissions::GUILD_ICON_PATH;
    let failed = RwSignal::new(None::<thiscord_shared::GuildIconId>);
    let source = Memo::new(move |_| icon_id.get().filter(|id| failed.get() != Some(*id)));
    view! {
        <span aria-hidden="true">{move || guild_initials(&name.get())}</span>
        {move || source.get().map(|id| view! {
            <img class="absolute inset-0 size-full object-cover" alt="" draggable="false"
                referrerpolicy="no-referrer" src=format!("{}{GUILD_ICON_PATH}/{id}", crate::account_client::api_base())
                on:error=move |_| failed.set(Some(id))/>
        })}
    }
}
