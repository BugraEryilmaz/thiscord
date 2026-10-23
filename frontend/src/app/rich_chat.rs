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
        // Cached jumps supersede pending context/older requests too.
        self.history_generation.update(|g| *g += 1);
        self.history_loading.set(false);
        self.rich.update(RichChat::cancel_context);
        self.highlight.set(Some(id));
        self.bottom.set(false);
        if self.messages.with_untracked(|m| m.get(id).is_some()) {
            self.anchor
                .set(Some(self.messages.with_untracked(|m| m.target_anchor(id))));
            self.scroll();
            return;
        }
        let generation = self.history_generation.get_untracked();
        let access = self.access_generation.get_untracked();
        let ticket = self
            .rich
            .try_update(RichChat::begin_context)
            .expect("live chat state");
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
            let snapshot = self.rich.try_update(|rich| rich.finish_context(ticket));
            if snapshot == Some(SnapshotStatus::Cancelled) {
                return;
            }
            match result {
                Ok(ChatResponse::History { history }) => {
                    if snapshot == Some(SnapshotStatus::Overflow) {
                        self.jump(id);
                        return;
                    }
                    // Reconcile against both resident rows and updates received
                    // outside the window BEFORE replacing the old cache.
                    self.bottom.set(false);
                    self.apply_batch(history.messages, Merge::Older, true);
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

    fn load_pins(self) {
        let ticket = self
            .rich
            .try_update(RichChat::begin_pins)
            .expect("live chat state");
        let generation = self.access_generation.get_untracked();
        leptos::task::spawn_local(async move {
            let result = self
                .request(&ChatRequest::Pins {
                    guild_id: self.guild,
                    channel_id: self.channel,
                })
                .await;
            if !self.alive() || self.access_generation.get_untracked() != generation {
                return;
            }
            match result {
                Ok(ChatResponse::Pins { messages }) => {
                    let status = self.rich.try_update(|rich| {
                        rich.finish_pins(ticket, messages, |id| {
                            self.messages
                                .with_untracked(|cache| cache.get(id).map(|m| m.get_untracked()))
                        })
                    });
                    if status == Some(SnapshotStatus::Overflow) {
                        self.load_pins();
                    }
                }
                result => {
                    if self.rich.try_update(|rich| rich.cancel_pins(ticket)) == Some(true)
                        && let Err(error) = result
                    {
                        self.status.set(error);
                    }
                }
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
    Effect::new(move |_| {
        chat.access_generation.track();
        chat.rich.update(RichChat::close_pins);
    });
    view! {<button class="mr-4 text-sm text-sky-300 underline" disabled=move ||chat.rich.with(RichChat::pins_loading) on:click=move |_|{
        if chat.rich.with_untracked(RichChat::pins_open){chat.rich.update(RichChat::close_pins);return;}
        chat.load_pins();
    }>{move ||crate::i18n::t("Pinned messages")}</button>
    <Show when=move ||chat.rich.with(RichChat::pins_open)><div class="max-h-64 overflow-auto border-y border-white/10 p-2"><p>{move ||crate::i18n::t("Pinned messages (up to 50)")}</p><Show when=move ||chat.rich.with(|r|r.pins().is_empty())><p>{move ||crate::i18n::t("No pinned messages")}</p></Show>
        <For each=move ||chat.rich.with(|r|r.pins().to_vec()) key=|m|(m.id,m.revision) children=move |m|view!{<button class="block w-full truncate p-2 text-left text-sm hover:bg-white/10" on:click=move |_|{chat.rich.update(RichChat::close_pins);chat.jump(m.id);}>{format!("{}: {}",m.display_name(),m.content.chars().take(160).collect::<String>())}</button>}/>
    </div></Show>}
}
