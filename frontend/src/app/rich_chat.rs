use super::*;
impl Chat {
    fn action(self, request: ChatRequest) {
        let generation = self.access_generation.get_untracked();
        leptos::task::spawn_local(async move {
            let result = self.request(&request).await;
            if !self.alive() || self.access_generation.get_untracked() != generation {
                return;
            }
            match result {
                Ok(ChatResponse::Message { message }) => self.merge(*message),
                Err(e) => self.status.set(e),
                _ => {}
            }
        });
    }
    pub(super) fn jump(self, id: MessageId) {
        self.highlight.set(Some(id));
        self.bottom.set(false);
        if self.messages.with_untracked(|m| m.get(id).is_some()) {
            self.anchor
                .set(Some(self.messages.with_untracked(|m| m.target_anchor(id))));
            self.scroll();
            return;
        }
        self.history_generation.update(|g| *g += 1);
        let generation = self.history_generation.get_untracked();
        let access = self.access_generation.get_untracked();
        self.history_loading.set(true);
        leptos::task::spawn_local(async move {
            let result = self
                .request(&ChatRequest::Context {
                    guild_id: self.guild,
                    channel_id: self.channel,
                    message_id: id,
                })
                .await;
            if !self.alive()
                || self.access_generation.get_untracked() != access
                || self.history_generation.get_untracked() != generation
            {
                return;
            }
            self.history_loading.set(false);
            match result {
                Ok(ChatResponse::History { history }) => {
                    self.messages.set(MessageCache::default());
                    self.merge_batch(history.messages, Merge::Older);
                    self.messages.update(|m| {
                        m.older = history.older;
                        m.newer = true;
                    });
                    self.anchor
                        .set(Some(self.messages.with_untracked(|m| m.target_anchor(id))));
                    self.scroll();
                }
                Err(error) => self.status.set(error),
                _ => {}
            }
        });
    }
}
#[component]
pub(super) fn Reply(chat: Chat, message: RwSignal<ChatMessage>) -> impl IntoView {
    view! {<Show when=move ||message.with(|m|m.reply.is_some())>
        <button class="my-1 block max-w-full truncate border-l-2 border-sky-400 pl-2 text-sm text-sky-300" disabled=move ||message.with(|m|m.reply.as_ref().is_none_or(|r|r.preview.is_none())) on:click=move |_|{
            if let Some(id)=message.with_untracked(|m|m.reply.as_ref().map(|r|r.id)) {chat.jump(id);}
        }>{move ||message.with(|m|m.reply.as_ref().map(|r|r.preview.clone().filter(|s|!s.is_empty()).unwrap_or_else(||if r.preview.is_some(){crate::i18n::t("Attachment message").into()}else{crate::i18n::t("Original message unavailable").into()})).unwrap_or_default())}</button>
    </Show>}
}
#[component]
pub(super) fn Actions(chat: Chat, message: RwSignal<ChatMessage>) -> impl IntoView {
    let busy = RwSignal::new(false);
    view! {<div class="my-1 flex flex-wrap items-center gap-2 text-xs">
        <Show when=move ||chat.permissions.with(|p|p.contains(&Permission::SendMessages))>
            <button class="rounded px-2 py-1 text-sky-300 hover:bg-white/10" on:click=move |_|chat.reply_to.set(Some(message.with_untracked(|m|m.id)))>{move ||crate::i18n::t("Reply")}</button>
        </Show>
        <Show when=move ||chat.permissions.with(|p|p.contains(&Permission::ManageMessages))>
            <button class="rounded px-2 py-1 hover:bg-white/10" on:click=move |_|message.with_untracked(|m|chat.action(ChatRequest::Pin{guild_id:chat.guild,channel_id:chat.channel,message_id:m.id,revision:m.revision,pinned:!m.pinned}))>{move ||message.with(|m|if m.pinned {crate::i18n::t("Unpin")}else{crate::i18n::t("Pin")})}</button>
        </Show>
        <For each=move || {REACTION_EMOJI.iter().map(|s|s.to_string()).collect::<Vec<_>>()} key=|e|e.clone() children=move |emoji|{
            let emoji=StoredValue::new(emoji);
            let reaction=Memo::new(move |_|message.with(|m|m.reactions.iter().find(|r|r.emoji==emoji.get_value()).cloned()));
            view!{<button class="rounded border border-white/10 px-2 py-1 hover:bg-white/10" class=("bg-brand/30",move ||reaction.with(|r|r.as_ref().is_some_and(|r|r.mine))) aria-pressed=move ||reaction.with(|r|r.as_ref().is_some_and(|r|r.mine)).to_string() aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "React {}",emoji.get_value()) disabled=move ||busy.get() || !chat.permissions.with(|p|p.contains(&Permission::SendMessages)) on:click=move |_|{
                busy.set(true);let id=message.with_untracked(|m|m.id);let present=!reaction.with_untracked(|r|r.as_ref().is_some_and(|r|r.mine));let generation=chat.access_generation.get_untracked();
                leptos::task::spawn_local(async move {
                    let result=chat.request(&ChatRequest::React{guild_id:chat.guild,channel_id:chat.channel,message_id:id,emoji:emoji.get_value(),present}).await;
                    if !chat.alive() || chat.access_generation.get_untracked()!=generation {return;}
                    busy.try_set(false);
                    match result {Ok(ChatResponse::Message{message})=>chat.merge(*message),Err(e)=>chat.status.set(e),_=>{}}
                });
            }>{emoji.get_value()}" "{move ||reaction.with(|r|r.as_ref().map(|r|r.count.to_string()).unwrap_or_default())}</button>}
        }/>
    </div>}
}
#[component]
pub(super) fn Pins(chat: Chat) -> impl IntoView {
    let open = RwSignal::new(false);
    let pins = RwSignal::new(Vec::<ChatMessage>::new());
    let loading = RwSignal::new(false);
    Effect::new(move |_| {
        chat.access_generation.track();
        open.set(false);
        pins.set(vec![]);
        loading.set(false);
    });
    Effect::new(move |_| {
        let changes = chat.rich_updates.get();
        if !open.get_untracked() {
            return;
        }
        pins.update(|pins| {
            for change in changes {
                if let Some(index) = pins.iter().position(|m| m.id == change.id) {
                    if change.revision <= pins[index].revision {
                        continue;
                    }
                    if !change.pinned || change.deleted {
                        pins.remove(index);
                    } else {
                        pins[index] = change;
                    }
                } else if change.pinned && !change.deleted && pins.len() < MAX_CHANNEL_PINS {
                    pins.push(change);
                }
            }
        });
    });
    view! {<button class="mr-4 text-sm text-sky-300 underline" disabled=move ||loading.get() on:click=move |_|{
        if open.get_untracked(){open.set(false);return;}
        loading.set(true);let generation=chat.access_generation.get_untracked();
        leptos::task::spawn_local(async move{
            let result=chat.request(&ChatRequest::Pins{guild_id:chat.guild,channel_id:chat.channel}).await;
            if !chat.alive() || chat.access_generation.get_untracked()!=generation{return;}
            loading.set(false);match result{Ok(ChatResponse::Pins{messages})=>{pins.set(messages);open.set(true);},Err(e)=>chat.status.set(e),_=>{}}
        });
    }>{move ||crate::i18n::t("Pinned messages")}</button>
    <Show when=move ||open.get()><div class="max-h-64 overflow-auto border-y border-white/10 p-2"><p>{move ||crate::i18n::t("Pinned messages (up to 50)")}</p><Show when=move ||pins.with(|p|p.is_empty())><p>{move ||crate::i18n::t("No pinned messages")}</p></Show>
        <For each=move ||pins.get() key=|m|(m.id,m.revision) children=move |m|view!{<button class="block w-full truncate p-2 text-left text-sm hover:bg-white/10" on:click=move |_|{open.set(false);chat.jump(m.id);}>{format!("{}: {}",m.display_name(),m.content.chars().take(160).collect::<String>())}</button>}/>
    </div></Show>}
}
