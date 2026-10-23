//! Search keeps one bounded page and one bounded context window.
use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{MessageId, Timestamp, pagination::PageCursor, search::*};

#[derive(Clone, Copy)]
struct State {
    ui: Ui,
    generation: RwSignal<u64>,
    loading: RwSignal<bool>,
    status: RwSignal<String>,
    rows: RwSignal<Vec<Hit>>,
    context: RwSignal<Vec<Hit>>,
    target: RwSignal<Option<MessageId>>,
    next: RwSignal<Option<PageCursor>>,
    submitted: RwSignal<Option<Filters>>,
}
impl State {
    fn clear(self) {
        self.generation.update(|g| *g += 1);
        self.rows.set(vec![]);
        self.context.set(vec![]);
        self.next.set(None);
        self.target.set(None);
        self.loading.set(false);
    }
    fn request(self, request: Request, context: bool) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        self.status.set("Searching…".into());
        let generation = self.generation.get_untracked();
        let token = self.ui.token.get_untracked();
        leptos::task::spawn_local(async move {
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
                || self.ui.token.get_untracked() != token
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
                            "Search complete"
                        }
                        .into(),
                    );
                    if context {
                        self.context.set(result.messages);
                    } else {
                        self.rows.set(result.messages);
                        self.next.set(result.next);
                        self.context.set(vec![]);
                        self.target.set(None);
                    }
                }
                futures_util::future::Either::Left((Err(error), _)) => {
                    self.rows.set(vec![]);
                    self.context.set(vec![]);
                    self.next.set(None);
                    self.status.set(error);
                }
                _ => self.status.set("Search timed out".into()),
            }
        });
    }
}
#[component]
pub(super) fn Launcher(ui: Ui, scope: Scope) -> impl IntoView {
    let scope = StoredValue::new(scope);
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let query = RwSignal::new(String::new());
    let person = RwSignal::new(String::new());
    let channel = RwSignal::new(String::new());
    let after = RwSignal::new(String::new());
    let before = RwSignal::new(String::new());
    let state = State {
        ui,
        generation: RwSignal::new(0),
        loading: RwSignal::new(false),
        status: RwSignal::new(String::new()),
        rows: RwSignal::new(vec![]),
        context: RwSignal::new(vec![]),
        target: RwSignal::new(None),
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
    let search = move || {
        let mut selected = scope.get_value();
        if let Scope::Guild { channel_id, .. } = &mut selected {
            *channel_id = if channel.get_untracked().is_empty() {
                None
            } else {
                channel.get_untracked().parse().ok()
            };
        }
        let date = |s: String| -> Result<Option<Timestamp>, String> {
            if s.is_empty() {
                Ok(None)
            } else {
                format!("{s}T00:00:00Z")
                    .parse()
                    .map(Some)
                    .map_err(|_| "Invalid date".into())
            }
        };
        let (Ok(after), Ok(before)) = (date(after.get_untracked()), date(before.get_untracked()))
        else {
            state.status.set("Invalid date".into());
            return;
        };
        let person = person.get_untracked();
        let filters = Filters {
            scope: selected,
            query: query.get_untracked(),
            person: (!person.trim().is_empty()).then(|| person.trim().to_string()),
            after,
            before,
        };
        if let Err(error) = filters.validate() {
            state.status.set(error.into());
            return;
        }
        state.clear();
        state.submitted.set(Some(filters.clone()));
        state.request(
            Request::Search {
                filters,
                cursor: None,
                limit: Default::default(),
            },
            false,
        );
    };
    view! {
        <button class="text-sm text-sky-300 underline" on:click=move |_|{if let Some(d)=dialog.get(){let _=d.show_modal();}}>{move ||crate::i18n::t("Search messages")}</button>
        <dialog node_ref=dialog class="m-auto w-full max-w-3xl rounded-xl border border-white/15 bg-slate-900 p-5 text-white backdrop:bg-black/70" on:close=move |_|state.clear() on:click=move |ev|super::dialog::dismiss_outside(ev,dialog,false)>
            <div class="mb-3 flex justify-between"><h2 class="text-xl font-semibold">{move ||crate::i18n::t("Search messages")}</h2><button aria-label=move ||crate::i18n::t("Close search") on:click=move |_|{if let Some(d)=dialog.get(){d.close();}}>{move ||crate::i18n::t("Close")}</button></div>
            <form class="space-y-2" on:submit=move |ev|{ev.prevent_default();search();}>
                <label class="block">{move ||crate::i18n::t("Words")}<input class="ml-2 rounded bg-black/30 p-2" maxlength="200" required prop:value=move ||query.get() on:input=move |ev|query.set(event_target_value(&ev))/></label>
                <div class="flex flex-wrap gap-3 text-sm"><label>{move ||crate::i18n::t("Person (@username)")}<input class="ml-2 rounded bg-black/30 p-2" maxlength="32" prop:value=move ||person.get() on:input=move |ev|person.set(event_target_value(&ev))/></label>
                    <Show when=move ||matches!(scope.get_value(),Scope::Guild{..})><label>{move ||crate::i18n::t("Channel")}<select class="ml-2 rounded bg-slate-800 p-2" on:change=move |ev|channel.set(event_target_value(&ev))><option value="">{move ||crate::i18n::t("All accessible channels")}</option>{move ||ui.server.with(|s|s.as_ref().map(|s|s.channels.iter().filter(|c|c.kind==thiscord_shared::permissions::ChannelKind::Text).map(|c|view!{<option value=c.id.to_string()>{c.name.clone()}</option>}).collect_view()))}</select></label></Show>
                    <label>{move ||crate::i18n::t("From (UTC)")}<input type="date" class="ml-2 bg-black/30 p-2" on:input=move |ev|after.set(event_target_value(&ev))/></label><label>{move ||crate::i18n::t("Before (UTC)")}<input type="date" class="ml-2 bg-black/30 p-2" on:input=move |ev|before.set(event_target_value(&ev))/></label>
                </div><button class="rounded bg-brand px-4 py-2" disabled=move ||state.loading.get()>{move ||crate::i18n::t("Search")}</button>
            </form><p class="my-2 text-sm text-white/60" role="status">{move ||crate::i18n::message(state.status.get())}</p>
            <div class="max-h-[50vh] space-y-3 overflow-auto">
                <For each=move ||state.rows.get() key=|m|(m.id,m.content.clone()) children=move |hit|{
                    let hit=StoredValue::new(hit);
                    view!{<article class="rounded border border-white/10 p-3"><p class="text-xs text-white/60">{move ||hit.with_value(|h|format!("@{} · {}",h.username,crate::i18n::datetime(h.created_at)))}</p><div>{hit.with_value(|h|super::message_content::render(&h.content,ui))}</div>
                        <button class="mt-2 text-sm text-sky-300 underline" disabled=move ||state.loading.get() on:click=move |_|{let h=hit.get_value();state.target.set(Some(h.id));state.context.set(vec![]);state.request(Request::Context{scope:h.scope,message_id:h.id},true);}>{move ||crate::i18n::t("View original and context")}</button>
                        <button class="ml-4 text-sm text-sky-300 underline" on:click=move |_|{let h=hit.get_value();match h.scope{
                            Scope::Guild{channel_id:Some(id),..}=>{
                                if let Some(ch)=ui.server.with_untracked(|s|s.as_ref().and_then(|s|s.channels.iter().find(|c|c.id==id).cloned())) {ui.chat_jump.set(Some((id,h.id)));ui.selected_channel.set(Some(ch));}
                            }
                            Scope::Private{conversation_id:Some(id)}=>super::friends::open(ui,id),_=>{}
                        }if let Some(d)=dialog.get(){d.close();}}>{move ||crate::i18n::t("Open channel")}</button>
                    </article>}
                }/>
                <Show when=move ||state.next.with(|c|c.is_some())><button class="text-sky-300 underline" disabled=move ||state.loading.get() on:click=move |_|{if let Some(filters)=state.submitted.get_untracked(){state.request(Request::Search{filters,cursor:state.next.get_untracked(),limit:Default::default()},false);}}>{move ||crate::i18n::t("Next page")}</button></Show>
                <Show when=move ||!state.context.with(|c|c.is_empty())><section class="border-t border-sky-400/50 pt-3"><h3>{move ||crate::i18n::t("Original message and nearby history")}</h3><For each=move ||state.context.get() key=|m|(m.id,m.content.clone()) children=move |m|view!{<article class="my-2 rounded p-2" class=("bg-brand/20",move ||state.target.get()==Some(m.id))><p class="text-xs text-white/60">{move ||format!("@{} · {}",m.username,crate::i18n::datetime(m.created_at))}</p><div>{super::message_content::render(&m.content,ui)}</div></article>}/></section></Show>
            </div>
        </dialog>
    }
}
