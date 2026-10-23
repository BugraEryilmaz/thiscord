use super::Chat;
use leptos::prelude::*;
use thiscord_frontend::mentions::{self, Query};
use thiscord_shared::{chat::*, permissions::Permission};

#[derive(Clone, Copy)]
pub(super) struct Picker {
    chat: Chat,
    draft: RwSignal<String>,
    pub input: NodeRef<leptos::html::Textarea>,
    context: RwSignal<Option<(String, Query)>>,
    dismissed: RwSignal<bool>,
    composing: RwSignal<bool>,
    completed: RwSignal<Option<(String, usize)>>,
    active: Memo<Option<Query>>,
    results: RwSignal<Option<(Query, Vec<MentionMember>)>>,
    selected: RwSignal<usize>,
    generation: RwSignal<u64>,
    error: RwSignal<bool>,
}

impl Picker {
    pub fn new(chat: Chat, draft: RwSignal<String>) -> Self {
        let context = RwSignal::new(None::<(String, Query)>);
        let dismissed = RwSignal::new(false);
        let composing = RwSignal::new(false);
        let active = Memo::new(move |_| {
            if dismissed.get()
                || composing.get()
                || !chat
                    .permissions
                    .with(|p| p.contains(&Permission::SendMessages))
            {
                return None;
            }
            context.with(|c| {
                c.as_ref()
                    .filter(|(text, _)| draft.with(|d| d == text))
                    .map(|(_, q)| q.clone())
            })
        });
        let picker = Self {
            chat,
            draft,
            input: NodeRef::new(),
            context,
            dismissed,
            composing,
            completed: RwSignal::new(None),
            active,
            results: RwSignal::new(None),
            selected: RwSignal::new(0),
            generation: RwSignal::new(0),
            error: RwSignal::new(false),
        };
        Effect::new(move |_| {
            let active = active.get();
            chat.access_generation.track();
            chat.ui.token.track();
            picker.generation.update(|g| *g += 1);
            let generation = picker.generation.get_untracked();
            picker.results.set(None);
            picker.selected.set(0);
            picker.error.set(false);
            let Some(query) = active else {
                return;
            };
            leptos::task::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(150).await;
                if !picker.current(generation, &query) {
                    return;
                }
                let response = chat
                    .request(&ChatRequest::MentionSuggestions {
                        guild_id: chat.guild,
                        channel_id: chat.channel,
                        query: query.text.clone(),
                    })
                    .await;
                if !picker.current(generation, &query) {
                    return;
                }
                match response {
                    Ok(ChatResponse::MentionSuggestions { members }) => {
                        picker.results.set(Some((query, members)))
                    }
                    _ => picker.error.set(true),
                }
            });
        });
        picker
    }

    fn current(self, generation: u64, query: &Query) -> bool {
        self.chat.alive()
            && self.generation.get_untracked() == generation
            && self.active.get_untracked().as_ref() == Some(query)
    }

    pub fn refresh(self) {
        let Some(input) = self.input.get_untracked() else {
            return;
        };
        let text = input.value();
        if self.completed.with_untracked(|completed| {
            completed
                .as_ref()
                .is_some_and(|(prefix, _)| !text.starts_with(prefix))
        }) {
            self.completed.set(None);
        }
        let query = input
            .selection_start()
            .ok()
            .flatten()
            .zip(input.selection_end().ok().flatten())
            .and_then(|(start, end)| mentions::query(&text, start, end));
        let query = query.filter(|q| {
            !self.completed.with_untracked(|completed| {
                completed.as_ref().is_some_and(|(prefix, at)| {
                    q.start == *at
                        && text.starts_with(prefix)
                        && q.start + 1 + q.text.len() >= prefix.len()
                })
            })
        });
        self.context.set(query.map(|q| (text, q)));
    }

    pub fn input(self) {
        self.dismissed.set(false);
        self.refresh();
    }

    pub fn composition(self, active: bool) {
        self.composing.set(active);
        if !active {
            self.input();
        }
    }

    pub fn dismiss(self) {
        self.dismissed.set(true);
    }

    fn members(self) -> Vec<MentionMember> {
        let active = self.active.get();
        self.results.with(|r| {
            r.as_ref()
                .filter(|(q, _)| Some(q) == active.as_ref())
                .map(|(_, members)| members.clone())
                .unwrap_or_default()
        })
    }

    fn choose(self, index: usize) {
        let Some(query) = self.active.get_untracked() else {
            return;
        };
        let Some(member) = self.members().get(index).cloned() else {
            return;
        };
        let Some(input) = self.input.get_untracked() else {
            return;
        };
        // Validate the actual caret again: a response cannot replace a different token.
        let current = input
            .selection_start()
            .ok()
            .flatten()
            .zip(input.selection_end().ok().flatten())
            .and_then(|(start, end)| mentions::query(&input.value(), start, end));
        if current.as_ref() != Some(&query) {
            self.dismiss();
            return;
        }
        if let Some((text, caret)) =
            mentions::insert(&self.draft.get_untracked(), &query, &member.username)
        {
            self.dismiss();
            let end = query.start + 1 + member.username.len();
            self.completed
                .set(Some((text[..end].to_owned(), query.start)));
            self.draft.set(text.clone());
            input.set_value(&text);
            let _ = input.focus();
            let _ = input.set_selection_range(caret, caret);
            self.chat.typing.set(true);
        }
    }

    /// True when completion consumed the key; Enter must not also send a message.
    pub fn keydown(self, event: &leptos::ev::KeyboardEvent) -> bool {
        if event.is_composing()
            || event.key_code() == 229
            || self.composing.get_untracked()
            || self.active.get_untracked().is_none()
        {
            return false;
        }
        let count = self.members().len();
        match event.key().as_str() {
            "Escape" => {
                self.dismiss();
            }
            "ArrowDown" | "ArrowUp" if count > 0 => {
                let step = if event.key() == "ArrowDown" {
                    1
                } else {
                    count - 1
                };
                self.selected.update(|i| *i = (*i + step) % count);
                if let (Some(option), Some(list)) = (
                    document().get_element_by_id(&format!(
                        "mention-option-{}",
                        self.selected.get_untracked()
                    )),
                    document().get_element_by_id("mention-suggestions"),
                ) {
                    let bounds = option.get_bounding_client_rect();
                    let viewport = list.get_bounding_client_rect();
                    if bounds.top() < viewport.top() {
                        option.scroll_into_view_with_bool(true);
                    } else if bounds.bottom() > viewport.bottom() {
                        option.scroll_into_view_with_bool(false);
                    }
                }
            }
            "Enter" | "Tab"
                if count > 0
                    && !event.shift_key()
                    && !event.ctrl_key()
                    && !event.alt_key()
                    && !event.meta_key() =>
            {
                if !event.repeat() {
                    self.choose(self.selected.get_untracked());
                }
            }
            // Avoid sending a partially typed mention while its suggestions load.
            "Enter"
                if !event.shift_key()
                    && self.results.get_untracked().is_none()
                    && !self.error.get_untracked() => {}
            _ => return false,
        }
        event.prevent_default();
        event.stop_propagation();
        true
    }

    pub fn expanded(self) -> bool {
        self.active.get().is_some()
    }
    pub fn active_descendant(self) -> Option<String> {
        (!self.members().is_empty()).then(|| format!("mention-option-{}", self.selected.get()))
    }
}

#[component]
pub(super) fn Suggestions(picker: Picker) -> impl IntoView {
    view! {
        <Show when=move || picker.expanded()>
            <div class="absolute bottom-full left-0 z-20 mb-2 w-full rounded-lg border border-white/15 bg-zinc-900 p-2 shadow-xl">
                <p class="px-2 py-1 text-xs text-white/50">{move || crate::i18n::t("Mention a member · ↑↓ to navigate · Enter or Tab to select")}</p>
                <ul id="mention-suggestions" role="listbox" aria-label=move || crate::i18n::t("Mention suggestions") class="max-h-64 overflow-y-auto">
                    {move || picker.members().into_iter().enumerate().map(|(index, member)| {
                        let name = if member.display_name.trim().is_empty() { member.username.clone() } else { member.display_name };
                        view! {
                            <li id=format!("mention-option-{index}") role="option" aria-selected=move || (picker.selected.get() == index).to_string()
                                class=move || if picker.selected.get() == index { "rounded-md bg-brand/25" } else { "rounded-md hover:bg-white/10" }>
                                <button type="button" tabindex="-1" class="flex w-full min-w-0 items-center justify-between gap-3 px-3 py-2 text-left"
                                    on:mousedown=move |event| event.prevent_default()
                                    on:click=move |_| picker.choose(index)>
                                    <span class="truncate font-medium">{name}</span>
                                    <span class="truncate text-sm text-white/50">{format!("@{}", member.username)}</span>
                                </button>
                            </li>
                        }
                    }).collect_view()}
                </ul>
                <Show when=move || picker.members().is_empty()>
                    <p role="status" class="px-3 py-2 text-sm text-white/60">{move || crate::i18n::t(if picker.error.get() {
                        "Could not load members. Keep typing or try again."
                    } else if picker.results.get().is_none() { "Searching members…" } else { "No matching members" })}</p>
                </Show>
            </div>
        </Show>
    }
}
