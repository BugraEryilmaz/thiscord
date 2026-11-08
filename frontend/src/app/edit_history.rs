//! The clickable "(edited)" marker and its edit-history dialog. History is
//! fetched on demand with the same authorization as reading the message and
//! is never cached: each opening re-reads it.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::{
    MessageId, Timestamp,
    chat::{EditHistory, MAX_EDIT_HISTORY, MessageVersion},
    ids::ConversationId,
};

/// Which API serves a message's history.
#[derive(Clone, Copy)]
pub(super) enum Scope {
    Guild {
        guild: thiscord_shared::GuildId,
        channel: thiscord_shared::ChannelId,
    },
    Direct(ConversationId),
}

async fn load(ui: Ui, scope: Scope, message: MessageId) -> Result<EditHistory, String> {
    match scope {
        Scope::Guild { guild, channel } => match crate::account_client::api_request(
            thiscord_shared::chat::CHAT_PATH,
            &thiscord_shared::chat::ChatRequest::EditHistory {
                guild_id: guild,
                channel_id: channel,
                message_id: message,
            },
            ui.token.get_untracked().as_deref(),
        )
        .await?
        {
            thiscord_shared::chat::ChatResponse::EditHistory { history } => Ok(history),
            _ => Err("Unexpected response".into()),
        },
        Scope::Direct(conversation) => match super::friends::request(
            ui,
            &thiscord_shared::friends::Request::EditHistory {
                conversation_id: conversation,
                message_id: message,
            },
        )
        .await?
        {
            thiscord_shared::friends::Response::EditHistory { history } => Ok(history),
            _ => Err("Unexpected response".into()),
        },
    }
}

/// "(edited)" after a message's text; hover shows the time, click opens history.
#[component]
pub(super) fn Edited(
    ui: Ui,
    scope: Scope,
    message: MessageId,
    #[prop(into)] edited_at: Signal<Option<Timestamp>>,
    #[prop(into)] created_at: Signal<Timestamp>,
    #[prop(into)] content: Signal<String>,
) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    // Guild versions resolve mentions against that guild's members.
    let guild = match scope {
        Scope::Guild { guild, .. } => Some(guild),
        Scope::Direct(_) => None,
    };
    let open = RwSignal::new(false);
    let history = RwSignal::new(None::<Vec<MessageVersion>>);
    let error = RwSignal::new(String::new());
    let generation = StoredValue::new(0u64);
    let tip = move || {
        edited_at.get().map(|at| {
            format!(
                "{}\n{}",
                thiscord_frontend::localized_format!(
                    crate::i18n::language(),
                    "Edited {}",
                    crate::i18n::datetime(at)
                ),
                crate::i18n::t("Click to see earlier versions")
            )
        })
    };
    let show = move |_| {
        generation.update_value(|g| *g += 1);
        let ticket = generation.get_value();
        history.set(None);
        error.set(String::new());
        open.set(true);
        leptos::task::spawn_local(async move {
            let result = load(ui, scope, message).await;
            if generation.try_get_value() != Some(ticket) || open.try_get_untracked() != Some(true)
            {
                return;
            }
            match result {
                Ok(loaded) => history.set(Some(loaded.versions)),
                Err(message) => error.set(message),
            }
        });
    };
    Effect::new(move |_| {
        if open.get()
            && let Some(d) = dialog.get()
        {
            super::dialog::open(&d);
        }
    });
    // Access loss (sign-out) closes the dialog and drops loaded versions.
    Effect::new(move |previous: Option<Option<String>>| {
        let token = ui.token.get();
        if previous.is_some_and(|p| p != token) {
            open.set(false);
            history.set(None);
        }
        token
    });
    let close = move || {
        if let Some(d) = dialog.get_untracked() {
            d.close();
        }
    };
    view! {
        <button type="button" class="edited-marker" aria-haspopup="dialog" data-tip=tip
            aria-label=move || edited_at.get().map(|at| thiscord_frontend::localized_format!(crate::i18n::language(), "Edited {}. Show edit history", crate::i18n::datetime(at))).unwrap_or_default()
            on:click=show>
            {move || format!("({})", crate::i18n::t("edited"))}
        </button>
        <Show when=move || open.get()>
            <dialog node_ref=dialog class="app-dialog edit-history max-w-xl" aria-labelledby=format!("edit-history-{message}")
                on:click=move |ev| super::dialog::dismiss_outside(ev, dialog, false)
                on:close=move |_| { open.set(false); history.set(None); }>
                <header class="mb-4 flex items-start justify-between gap-4">
                    <span class="dialog-mark"><Icon kind=Glyph::History/></span>
                    <h2 id=format!("edit-history-{message}") class="dialog-title mr-auto self-center">{move || crate::i18n::t("Edit history")}</h2>
                    <button type="button" class="icon-button" autofocus aria-label=move || crate::i18n::t("Close edit history") on:click=move |_| close()><Icon kind=Glyph::Close/></button>
                </header>
                <ol class="edit-history-list" aria-label=move || crate::i18n::t("Message versions")>
                    <li class="edit-version edit-version-current">
                        <p class="edit-version-meta">
                            <span class="edit-version-badge">{move || crate::i18n::t("Current")}</span>
                            <time>{move || edited_at.get().map(|at| thiscord_frontend::localized_format!(crate::i18n::language(), "Edited {}", crate::i18n::datetime(at))).unwrap_or_default()}</time>
                        </p>
                        <div class="message-text whitespace-pre-wrap break-words">{move || super::message_content::render(&content.get(), ui, guild)}</div>
                    </li>
                    {move || history.get().map(|versions| {
                        let count = versions.len();
                        versions.into_iter().enumerate().map(|(index, version)| {
                            let original = index + 1 == count && count < MAX_EDIT_HISTORY;
                            let at = version.created_at;
                            let sent = original && at == created_at.get_untracked();
                            view! {
                                <li class="edit-version">
                                    <p class="edit-version-meta">
                                        <time>{if sent {
                                            thiscord_frontend::localized_format!(crate::i18n::language(), "Sent {}", crate::i18n::datetime(at))
                                        } else {
                                            thiscord_frontend::localized_format!(crate::i18n::language(), "Edited {}", crate::i18n::datetime(at))
                                        }}</time>
                                        <span aria-hidden="true">" → "</span>
                                        <time class="text-muted">{thiscord_frontend::localized_format!(crate::i18n::language(), "replaced {}", crate::i18n::clock(version.replaced_at))}</time>
                                    </p>
                                    <div class="message-text whitespace-pre-wrap break-words">{super::message_content::render(&version.content, ui, guild)}</div>
                                </li>
                            }
                        }).collect_view()
                    })}
                </ol>
                <p class="dialog-footnote mt-4" role="status">
                    {move || {
                        if !error.get().is_empty() {
                            crate::i18n::message(error.get())
                        } else {
                            match history.get() {
                                None => crate::i18n::t("Loading…").into(),
                                Some(v) if v.is_empty() => crate::i18n::t("Earlier versions of this message weren't kept.").into(),
                                Some(v) if v.len() >= MAX_EDIT_HISTORY => thiscord_frontend::localized_format!(crate::i18n::language(), "Only the last {} versions are kept.", MAX_EDIT_HISTORY),
                                Some(_) => crate::i18n::t("Deleting the message deletes its edit history.").into(),
                            }
                        }
                    }}
                </p>
            </dialog>
        </Show>
    }
}
