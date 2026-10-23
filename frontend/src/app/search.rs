//! Conversation-local search shares the history area and keeps one bounded page.
use super::{
    Ui,
    icons::{Glyph, Icon},
};
use leptos::prelude::*;
use thiscord_shared::{pagination::PageCursor, search::*};

#[derive(Clone, Copy)]
pub(super) struct State {
    token: RwSignal<Option<String>>,
    scope: StoredValue<Scope>,
    pub active: RwSignal<bool>,
    input: NodeRef<leptos::html::Input>,
    launcher: NodeRef<leptos::html::Button>,
    query: RwSignal<String>,
    generation: RwSignal<u64>,
    loading: RwSignal<bool>,
    status: RwSignal<String>,
    rows: RwSignal<Vec<Hit>>,
    next: RwSignal<Option<PageCursor>>,
    submitted: RwSignal<Option<Filters>>,
}
impl State {
    pub fn new(ui: Ui, scope: Scope) -> Self {
        let state = Self {
            token: ui.token,
            scope: StoredValue::new(scope),
            active: RwSignal::new(false),
            input: NodeRef::new(),
            launcher: NodeRef::new(),
            query: RwSignal::new(String::new()),
            generation: RwSignal::new(0),
            loading: RwSignal::new(false),
            status: RwSignal::new(String::new()),
            rows: RwSignal::new(vec![]),
            next: RwSignal::new(None),
            submitted: RwSignal::new(None),
        };
        let guild_revision = Memo::new(move |_| {
            ui.server
                .with(|s| s.as_ref().map(|s| (s.guild.id, s.guild.revision)))
        });
        let private_consent = Memo::new(move |_| {
            ui.friends.snapshot.with(|s| {
                s.friends
                    .iter()
                    .filter(|f| f.relationship == thiscord_shared::friends::Relationship::Friend)
                    .map(|f| f.conversation_id)
                    .collect::<Vec<_>>()
            })
        });
        Effect::new(move |_| {
            ui.token.track();
            guild_revision.track();
            private_consent.track();
            ui.socket_status.track();
            ui.friends.connected.track();
            state.clear();
        });
        let key = window_event_listener(leptos::ev::keydown, move |event| {
            if event.is_composing()
                || event.default_prevented()
                || document()
                    .query_selector("dialog[open]")
                    .ok()
                    .flatten()
                    .is_some()
            {
                return;
            }
            if (event.ctrl_key() || event.meta_key())
                && !event.alt_key()
                && event.key().eq_ignore_ascii_case("f")
            {
                event.prevent_default();
                state.open();
            } else if event.key() == "Escape" && state.active.get_untracked() {
                event.prevent_default();
                state.close();
            }
        });
        on_cleanup(move || key.remove());
        state
    }
    fn clear(self) {
        self.generation.update(|g| *g += 1);
        self.rows.set(vec![]);
        self.next.set(None);
        self.submitted.set(None);
        self.loading.set(false);
        self.status
            .set("Enter words to search this conversation".into());
    }
    fn open(self) {
        self.active.set(true);
        request_animation_frame(move || {
            if let Some(input) = self.input.get_untracked() {
                let _ = input.focus();
                input.select();
            }
        });
    }
    pub fn close(self) {
        self.active.set(false);
        self.clear();
        if let Some(button) = self.launcher.get_untracked() {
            let _ = button.focus();
        }
    }
    fn search(self) {
        let filters = Filters {
            scope: self.scope.get_value(),
            query: self.query.get_untracked(),
            person: None,
            after: None,
            before: None,
        };
        self.clear();
        if let Err(error) = filters.validate() {
            self.status.set(error.into());
            return;
        }
        self.submitted.set(Some(filters.clone()));
        self.request(filters, None);
    }
    fn request(self, filters: Filters, cursor: Option<PageCursor>) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        self.status.set("Searching…".into());
        let generation = self.generation.get_untracked();
        let token = self.token.get_untracked();
        leptos::task::spawn_local(async move {
            let request = Request::Search {
                filters,
                cursor,
                limit: Default::default(),
            };
            let result = futures_util::future::select(
                Box::pin(crate::account_client::api_request::<_, Response>(
                    SEARCH_PATH,
                    &request,
                    token.as_deref(),
                )),
                Box::pin(gloo_timers::future::TimeoutFuture::new(15000)),
            )
            .await;
            if self.generation.try_get_untracked() != Some(generation)
                || self.token.get_untracked() != token
            {
                return;
            }
            self.loading.set(false);
            match result {
                futures_util::future::Either::Left((Ok(result), _)) => {
                    self.status.set(
                        if result.messages.is_empty() {
                            "No messages found"
                        } else {
                            "Select a message to jump to it"
                        }
                        .into(),
                    );
                    self.rows.set(result.messages);
                    self.next.set(result.next);
                }
                futures_util::future::Either::Left((Err(error), _)) => {
                    self.rows.set(vec![]);
                    self.next.set(None);
                    self.status.set(error);
                }
                _ => self.status.set("Search timed out".into()),
            }
        });
    }
}

#[component]
pub(super) fn Launcher(state: State) -> impl IntoView {
    view! {
        <button node_ref=state.launcher class="icon-button" aria-label=move ||crate::i18n::t("Search messages")
            title=move ||crate::i18n::t("Search messages (Ctrl+F / ⌘F)")
            aria-expanded=move ||state.active.get().to_string() on:click=move |_|state.open()>
            <Icon kind=Glyph::Search/>
        </button>
    }
}

#[component]
pub(super) fn Bar(state: State) -> impl IntoView {
    view! {
        <Show when=move ||state.active.get()>
            <form role="search" class="flex shrink-0 items-center gap-2 border-b border-white/10 px-4 py-3"
                on:submit=move |event|{event.prevent_default();state.search();}>
                <Icon kind=Glyph::Search/>
                <input node_ref=state.input type="search" class="min-w-0 flex-1 rounded bg-black/20 px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-brand"
                    aria-label=move ||crate::i18n::t("Search this conversation") placeholder=move ||crate::i18n::t("Search this conversation")
                    maxlength="200" required prop:value=move ||state.query.get()
                    on:input=move |event|{state.query.set(event_target_value(&event));state.clear();}/>
                <button class="rounded bg-brand px-3 py-2 text-sm">{move ||crate::i18n::t("Search")}</button>
                <button type="button" class="icon-button" aria-label=move ||crate::i18n::t("Close search") title="Esc" on:click=move |_|state.close()><Icon kind=Glyph::Close/></button>
            </form>
        </Show>
    }
}

#[component]
pub(super) fn Results(state: State, on_jump: Callback<Hit>) -> impl IntoView {
    let list = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        state.rows.track();
        if let Some(list) = list.get() {
            list.set_scroll_top(0);
        }
    });
    view! {
        <Show when=move ||state.active.get()>
            <div node_ref=list class="min-h-0 flex-1 overflow-y-auto px-4 py-3" aria-busy=move ||state.loading.get().to_string()>
                <p class="mb-4 text-sm text-white/50" role="status">{move ||crate::i18n::message(state.status.get())}</p>
                <ol aria-label=move ||crate::i18n::t("Search results")>
                    <For each=move ||state.rows.get() key=|hit|(hit.id,hit.content.clone()) children=move |hit| {
                        let hit = StoredValue::new(hit);
                        view! {
                            <li><button type="button" class="mb-1 block w-full rounded px-3 py-3 text-left hover:bg-white/5 focus-visible:bg-white/5"
                                on:click=move |_|{let hit=hit.get_value();state.close();on_jump.run(hit);}>
                                <span class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                                    <span class="font-semibold">{hit.with_value(|h|format!("@{}",h.username))}</span>
                                    <time class="text-xs text-white/45">{move ||hit.with_value(|h|crate::i18n::datetime(h.created_at))}</time>
                                </span>
                                <span class="mt-1 block whitespace-pre-wrap break-words text-sm text-content">{hit.with_value(|h|h.content.clone())}</span>
                            </button></li>
                        }
                    }/>
                </ol>
                <Show when=move ||state.next.get().is_some()>
                    <button class="my-3 rounded bg-white/5 px-4 py-2 text-sm hover:bg-white/10 disabled:opacity-50" disabled=move ||state.loading.get()
                        on:click=move |_|{if let Some(filters)=state.submitted.get_untracked(){state.request(filters,state.next.get_untracked());}}>{move ||crate::i18n::t("Next page")}</button>
                </Show>
            </div>
        </Show>
    }
}
