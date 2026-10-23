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

    pub(super) fn load_pins(self) {
        if !self.ready.get_untracked()
            || !self
                .permissions
                .with_untracked(|p| p.contains(&Permission::ReadHistory))
            || self.rich.with_untracked(RichChat::pins_loading)
        {
            return;
        }
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
/// A small button panel with the same surface as the app's context menus.
/// Listeners exist only while open and are released with the virtualized row.
#[component]
fn ActionPopover(
    chat: Chat,
    label: &'static str,
    icon: Glyph,
    children: Children,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let group = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let panel = NodeRef::<leptos::html::Div>::new();
    let position = RwSignal::new((8.0, 8.0));
    let dismiss = move || {
        if let Some(button) = trigger.get_untracked() {
            let _ = button.focus();
        }
        open.set(false);
    };
    Effect::new(move |_| {
        chat.viewport.track();
        chat.access_generation.track();
        open.set(false);
    });
    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        if let (Some(button), Some(panel)) = (trigger.get(), panel.get()) {
            // Reveal before measuring: the reactive hidden attribute can update
            // after this effect, otherwise the first open measures a zero box.
            panel.set_hidden(false);
            let anchor = button.get_bounding_client_rect();
            let size = panel.get_bounding_client_rect();
            let w = window()
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(800.0);
            let h = window()
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(600.0);
            let y = if anchor.bottom() + 6.0 + size.height() <= h - 8.0 {
                anchor.bottom() + 6.0
            } else {
                anchor.top() - size.height() - 6.0
            };
            position.set((
                (anchor.right() - size.width()).clamp(8.0, (w - size.width() - 8.0).max(8.0)),
                y.clamp(8.0, (h - size.height() - 8.0).max(8.0)),
            ));
            if let Ok(Some(first)) = panel.query_selector("button:not(:disabled)")
                && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
            {
                let _ = first.focus();
            }
        }
        let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
            let inside = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|target| {
                    group
                        .get_untracked()
                        .is_some_and(|group| group.contains(Some(&target)))
                });
            if !inside {
                open.set(false);
            }
        });
        let resize = window_event_listener(leptos::ev::resize, move |_| open.set(false));
        on_cleanup(move || {
            outside.remove();
            resize.remove();
        });
    });
    view! {
        <div node_ref=group
            on:focusout=move |e| {
                let inside = e.related_target().and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                    .is_some_and(|target| group.get_untracked().is_some_and(|group| group.contains(Some(&target))));
                if !inside { open.set(false); }
            }
            on:keydown=move |e| {
                if e.key() == "Escape" && open.get_untracked() {
                    e.prevent_default(); e.stop_propagation(); dismiss();
                }
            }>
            <button node_ref=trigger type="button" class="icon-button" aria-label=move ||crate::i18n::t(label)
                data-tip=move ||crate::i18n::t(label) aria-expanded=move ||open.get().to_string()
                on:click=move |_|open.update(|value| *value = !*value)><Icon kind=icon/></button>
            <div node_ref=panel class="message-popover" hidden=move ||!open.get() role="group" aria-label=move ||crate::i18n::t(label)
                style:left=move ||format!("{}px",position.get().0) style:top=move ||format!("{}px",position.get().1)
                on:click=move |e| {
                    if e.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                        .and_then(|e| e.closest("button").ok().flatten()).is_some() { dismiss(); }
                }>
                {children()}
            </div>
        </div>
    }
}

#[component]
fn ReactionButton(
    chat: Chat,
    message: RwSignal<ChatMessage>,
    busy: RwSignal<bool>,
    emoji: String,
    #[prop(default = false)] count: bool,
) -> impl IntoView {
    let emoji = StoredValue::new(emoji);
    let reaction = Memo::new(move |_| {
        message.with(|m| {
            m.reactions
                .iter()
                .find(|r| r.emoji == emoji.get_value())
                .cloned()
        })
    });
    view! {
        <button type="button" aria-pressed=move ||reaction.with(|r|r.as_ref().is_some_and(|r|r.mine)).to_string()
            aria-label=move ||thiscord_frontend::localized_format!(crate::i18n::language(), "React {}", emoji.get_value())
            disabled=move ||busy.get() || !chat.permissions.with(|p|p.contains(&Permission::SendMessages))
            on:click=move |_| {
                busy.set(true);
                let id = message.with_untracked(|m|m.id);
                let present = !reaction.with_untracked(|r|r.as_ref().is_some_and(|r|r.mine));
                let generation = chat.access_generation.get_untracked();
                leptos::task::spawn_local(async move {
                    let result = chat.request(&ChatRequest::React { guild_id:chat.guild, channel_id:chat.channel, message_id:id, emoji:emoji.get_value(), present }).await;
                    if !chat.alive() || chat.access_generation.get_untracked()!=generation { return; }
                    busy.try_set(false);
                    match result { Ok(ChatResponse::Message { message }) => chat.merge(*message), Err(e) => chat.status.set(e), _ => {} }
                });
            }>
            <span>{emoji.get_value()}</span>
            {count.then(||view! {<span>{move ||reaction.with(|r|r.as_ref().map(|r|r.count.to_string()).unwrap_or_default())}</span>})}
        </button>
    }
}

#[component]
pub(super) fn Actions(
    chat: Chat,
    message: RwSignal<ChatMessage>,
    editing: RwSignal<Option<MessageId>>,
    draft: RwSignal<String>,
) -> impl IntoView {
    let busy = RwSignal::new(false);
    let own = move || {
        message.with(|m| {
            chat.ui
                .account
                .with(|a| a.as_ref().is_some_and(|a| m.author_id == Some(a.id)))
        })
    };
    let can_send = move || {
        chat.permissions
            .with(|p| p.contains(&Permission::SendMessages))
    };
    let can_edit = move || {
        own()
            && chat
                .permissions
                .with(|p| p.contains(&Permission::EditOwnMessages))
    };
    let can_delete = move || {
        chat.permissions.with(|p| {
            p.contains(&Permission::ManageMessages)
                || (own() && p.contains(&Permission::DeleteOwnMessages))
        })
    };
    let can_pin = move || {
        chat.permissions
            .with(|p| p.contains(&Permission::ManageMessages))
    };
    let has_more = move || !own() || can_edit() || can_delete() || can_pin();
    view! {
        <Show when=move ||can_send() || has_more()>
            <div class="message-actions" role="group" aria-label=move ||crate::i18n::t("Message actions")>
                <Show when=can_send>
                    <button type="button" class="icon-button" aria-label=move ||crate::i18n::t("Reply") data-tip=move ||crate::i18n::t("Reply")
                        on:click=move |_|chat.reply_to.set(Some(message.with_untracked(|m|m.id)))><Icon kind=Glyph::Reply/></button>
                    <ActionPopover chat label="Add reaction" icon=Glyph::Reaction>
                        <div class="message-reaction-picker">
                            {REACTION_EMOJI.iter().map(|emoji| view! {<ReactionButton chat message busy emoji=emoji.to_string()/>}).collect_view()}
                        </div>
                    </ActionPopover>
                </Show>
                <Show when=has_more>
                    <ActionPopover chat label="More message options" icon=Glyph::More>
                        <div class="message-options">
                            <Show when=can_pin>
                                <button type="button" class="context-item" on:click=move |_|message.with_untracked(|m|chat.action(ChatRequest::Pin {guild_id:chat.guild,channel_id:chat.channel,message_id:m.id,revision:m.revision,pinned:!m.pinned}))>
                                    <Icon kind=Glyph::Pin/>{move ||message.with(|m|if m.pinned {crate::i18n::t("Unpin")}else{crate::i18n::t("Pin")})}
                                </button>
                            </Show>
                            <Show when=can_edit>
                                <button type="button" class="context-item" on:click=move |_| {
                                    editing.set(Some(message.with_untracked(|m|m.id)));
                                    draft.set(message.with_untracked(|m|m.content.clone()));
                                }><Icon kind=Glyph::Edit/>{move ||crate::i18n::t("Edit message")}</button>
                            </Show>
                            <Show when=move ||!own()>
                                <button type="button" class="context-item" on:click=move |_|chat.ui.friends.report_target.set(Some(thiscord_shared::safety::ReportTarget::GuildMessage {guild_id:chat.guild,channel_id:chat.channel,message_id:message.with_untracked(|m|m.id)}))>
                                    <Icon kind=Glyph::Flag/>{move ||crate::i18n::t("Report")}
                                </button>
                            </Show>
                            <Show when=can_delete>
                                <button type="button" class="context-item context-danger" on:click=move |_| {
                                    if !window().confirm_with_message(crate::i18n::t("Delete this message?")).unwrap_or(false) { return; }
                                    message.with_untracked(|m|chat.action(ChatRequest::Delete {guild_id:chat.guild,channel_id:chat.channel,message_id:m.id,revision:m.revision}));
                                }><Icon kind=Glyph::Trash/>{move ||crate::i18n::t("Delete message")}</button>
                            </Show>
                        </div>
                    </ActionPopover>
                </Show>
            </div>
        </Show>
        <Show when=move ||message.with(|m|!m.reactions.is_empty())>
            <div class="message-reactions">
                <For each=move ||message.with(|m|m.reactions.iter().filter(|r|r.count > 0).map(|r|r.emoji.clone()).collect::<Vec<_>>()) key=|emoji|emoji.clone()
                    children=move |emoji|view! {<ReactionButton chat message busy emoji count=true/>}/>
            </div>
        </Show>
    }
}
#[component]
pub(super) fn Pins(chat: Chat) -> impl IntoView {
    view! {
        <Show when=move || !chat.searching.get() && chat.ready.get()
            && chat.permissions.with(|p|p.contains(&Permission::ReadHistory))
            && chat.rich.with(|r|!r.pins_open() || !r.pins().is_empty())>
            <section class="max-h-[30vh] shrink-0 overflow-y-auto overscroll-contain border-b border-white/10 bg-surface"
                aria-label=move ||crate::i18n::t("Pinned messages")>
                <Show when=move ||chat.rich.with(RichChat::pins_loading)>
                    <p class="px-4 py-2 text-xs text-white/50" role="status">{move ||crate::i18n::t("Loading pinned messages…")}</p>
                </Show>
                <Show when=move ||chat.rich.with(|r|!r.pins_open() && !r.pins_loading())>
                    <button class="m-3 rounded px-3 py-2 text-sm text-sky-300 hover:bg-white/5" on:click=move |_|chat.load_pins()>{move ||crate::i18n::t("Retry pinned messages")}</button>
                </Show>
                <For each=move ||chat.rich.with(|r|r.pins().iter().map(|m|m.id).collect::<Vec<_>>()) key=|id|*id
                    children=move |id|view!{<PinnedMessage chat id/>}/>
            </section>
        </Show>
    }.into_any()
}

#[component]
fn PinnedMessage(chat: Chat, id: MessageId) -> impl IntoView {
    let message = Signal::derive(move || {
        chat.rich
            .with(|r| r.pins().iter().find(|m| m.id == id).cloned())
    });
    view! {
        <article class="border-b border-white/5 px-4 py-3 last:border-b-0" data-pinned-message=id.to_string()>
            <div class="mb-1 flex items-center gap-2">
                <span class="text-brand" aria-label=move ||crate::i18n::t("Pinned")><Icon kind=Glyph::Pin/></span>
                <button class="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-3 text-left hover:underline"
                    title=move ||crate::i18n::t("Jump to pinned message") on:click=move |_|chat.jump(id)>
                    <span class="truncate text-sm font-semibold">{move ||message.with(|m|m.as_ref().map(|m|m.display_name().to_owned()))}</span>
                    <time class="text-xs text-white/45">{move ||message.with(|m|m.as_ref().map(|m|crate::i18n::datetime(m.created_at)))}</time>
                </button>
                <button class="icon-button shrink-0" aria-label=move ||crate::i18n::t("Jump to pinned message") title=move ||crate::i18n::t("Jump to pinned message") on:click=move |_|chat.jump(id)><Icon kind=Glyph::Join/></button>
                <Show when=move ||chat.permissions.with(|p|p.contains(&Permission::ManageMessages))>
                    <button class="icon-button shrink-0" aria-label=move ||crate::i18n::t("Unpin") title=move ||crate::i18n::t("Unpin") on:click=move |_|{
                        if let Some(message)=message.get_untracked(){chat.action(ChatRequest::Pin{guild_id:chat.guild,channel_id:chat.channel,message_id:id,revision:message.revision,pinned:false});}
                    }><Icon kind=Glyph::Close/></button>
                </Show>
            </div>
            <div class="pl-7 text-sm text-content">{move ||message.with(|m|super::super::message_content::render(m.as_ref().map(|m|m.content.as_str()).unwrap_or_default(),chat.ui))}</div>
            <For each=move ||message.with(|m|m.as_ref().map(|m|m.attachments.clone()).unwrap_or_default()) key=|a|a.id children=move |attachment|view!{
                <button class="ml-7 mt-2 block max-w-full truncate text-sm text-sky-300 hover:underline" title=move ||crate::i18n::t("Jump to pinned message") on:click=move |_|chat.jump(id)>{attachment.filename}</button>
            }/>
        </article>
    }.into_any()
}
