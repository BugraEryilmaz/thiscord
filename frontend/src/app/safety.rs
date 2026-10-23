use super::Ui;
use leptos::prelude::*;
use thiscord_shared::{
    AccountId,
    friends::Person,
    ids::ReportId,
    pagination::PageRequest,
    permissions::{PermissionRequest, PermissionResponse},
    safety::*,
};

async fn request(ui: Ui, command: &Request) -> Result<Response, String> {
    let token = ui.token.get_untracked();
    let result = crate::account_client::api_request(SAFETY_PATH, command, token.as_deref()).await;
    if token != ui.token.get_untracked() {
        return Err("Session changed".into());
    }
    result
}
pub(super) fn block(ui: Ui, account_id: AccountId) {
    if !window().confirm_with_message(crate::i18n::t("Block this account? Friendship requests, private messages and calls will stop. Unblocking will not restore friendship. Shared server interactions stay visible.")).unwrap_or(false){return;}
    leptos::task::spawn_local(async move {
        match request(ui, &Request::Block { account_id }).await {
            Ok(_) => {
                ui.friends.selected.set(None);
                ui.status
                    .set("Account blocked. Manage blocked accounts in Safety & reports.".into());
            }
            Err(e) => ui.status.set(e),
        }
    });
}
#[component]
pub(super) fn ReportButton(ui: Ui, target: ReportTarget) -> impl IntoView {
    let target = StoredValue::new(target);
    view! {<button type="button" class="text-xs text-white/60 hover:text-white" on:click=move |_|ui.friends.report_target.set(Some(target.get_value()))>{move ||crate::i18n::t("Report")}</button>}
}
#[component]
pub(super) fn PersonActions(ui: Ui, account_id: AccountId) -> impl IntoView {
    view! {<div class="flex gap-3 text-xs"><button type="button" class="text-red-200" on:click=move |_|block(ui,account_id)>{move ||crate::i18n::t("Block")}</button><ReportButton ui target=ReportTarget::User {account_id}/></div>}
}
#[component]
pub(super) fn ReportHost(ui: Ui) -> impl IntoView {
    view! {<Show when=move ||ui.friends.report_target.get().is_some()><ReportDialog ui target=ui.friends.report_target.get_untracked().unwrap()/></Show>}
}
#[component]
fn ReportDialog(ui: Ui, target: ReportTarget) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let reason = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let target = StoredValue::new(target);
    let id = StoredValue::new(
        window()
            .crypto()
            .ok()
            .and_then(|c| c.random_uuid().parse::<ReportId>().ok()),
    );
    let frozen = StoredValue::new(None::<String>);
    Effect::new(move |_| {
        if let Some(dialog) = dialog.get() {
            let _ = dialog.show_modal();
        }
    });
    let close = move || {
        if !busy.get_untracked() {
            ui.friends.report_target.set(None);
        }
    };
    view! {<dialog node_ref=dialog class="m-auto w-[calc(100%-2rem)] max-w-xl space-y-4 rounded-xl border border-white/15 bg-surface p-6 text-white backdrop:bg-black/70" on:cancel=move |ev:leptos::ev::Event|{ev.prevent_default();close();}>
        <h2 class="text-xl font-semibold">{move ||crate::i18n::t("Submit a report")}</h2>
        <p class="text-sm text-white/60">{move ||crate::i18n::t(match target.get_value(){ReportTarget::GuildMessage {..}=>"This message and your explanation will be shared with moderators who can currently manage messages in this server channel.",ReportTarget::DirectMessage {..}=>"You are explicitly sharing this one private message, its author, timestamp and attachment names with instance reviewers. Other messages and private attachment bytes are not shared.",ReportTarget::User {..}=>"Your explanation and the reported account ID will be shared with instance reviewers. Do not include unrelated private information."})}</p>
        <form class="space-y-3" on:submit=move |ev|{ev.prevent_default();if busy.get_untracked(){return;}let Some(id)=id.get_value()else{error.set("Cannot create report ID".into());return;};let reason=match thiscord_frontend::safety::submit_reason(&reason.get_untracked(),frozen.get_value().as_deref()){Ok(reason)=>reason,Err(e)=>{error.set(e.into());return;}};frozen.set_value(Some(reason.clone()));busy.set(true);error.set(String::new());leptos::task::spawn_local(async move{let result=request(ui,&Request::CreateReport {id,target:target.get_value(),reason}).await;if busy.is_disposed(){return;}busy.set(false);match result {Ok(_)=>{ui.friends.report_target.set(None);ui.status.set("Report submitted. Track it in Safety & reports.".into());},Err(e)=>error.set(e)}});}>
            <label class="block text-sm">{move ||crate::i18n::t("What happened?")}<textarea class="mt-2 min-h-28 w-full rounded bg-white/10 p-3" maxlength="2000" required=true readonly=move ||busy.get()||frozen.get_value().is_some() prop:value=move ||reason.get() on:input=move |ev|reason.set(event_target_value(&ev))/></label>
            <p role="alert" class="text-red-200">{move ||crate::i18n::message(error.get())}</p>
            <button class="rounded bg-brand px-4 py-2" disabled=move ||busy.get()>{move ||crate::i18n::t("Submit report")}</button><button type="button" class="ml-3" disabled=move ||busy.get() on:click=move |_|close()>{move ||crate::i18n::t("Cancel")}</button>
        </form>
    </dialog>}
}
#[component]
pub(super) fn Panel(ui: Ui) -> impl IntoView {
    let blocks = RwSignal::new(Vec::<Person>::new());
    let reports = RwSignal::new(Vec::<Report>::new());
    let inbox = RwSignal::new(Inbox::Mine);
    let next = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let load = move |more: bool| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let selected = inbox.get_untracked();
        let after = if more { next.get_untracked() } else { None };
        leptos::task::spawn_local(async move {
            let result = request(
                ui,
                &Request::Reports {
                    inbox: selected,
                    page: PageRequest {
                        after,
                        ..Default::default()
                    },
                },
            )
            .await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            if inbox.get_untracked() != selected {
                return;
            }
            match result {
                Ok(Response::Reports { page }) => {
                    reports.set(page.items);
                    next.set(page.next_cursor);
                }
                Err(e) => error.set(e),
                _ => {}
            }
        });
    };
    leptos::task::spawn_local(async move {
        match request(ui, &Request::Blocks).await {
            Ok(Response::Blocks { people }) if !blocks.is_disposed() => blocks.set(people),
            Err(e) if !error.is_disposed() => error.set(e),
            _ => {}
        }
    });
    Effect::new(move |_| {
        inbox.track();
        load(false);
    });
    view! {<div class="space-y-6">
        <section class="settings-card space-y-3"><h3>{move ||crate::i18n::t("Blocked accounts")}</h3><p class="text-sm text-white/60">{move ||crate::i18n::t("Blocking ends friendship and private calls. Neither person can re-request or access private history until unblocked and a new request is accepted. Shared servers, moderation permissions and server notifications are unchanged.")}</p>
        <For each=move ||blocks.get() key=|p|p.id children=move |p|view!{<div class="flex justify-between gap-3"><span>{format!("{} (@{})",p.display_name,p.username)}</span><button class="text-brand" on:click=move |_|leptos::task::spawn_local(async move{match request(ui,&Request::Unblock {account_id:p.id}).await {Ok(_)=>{if !blocks.is_disposed(){blocks.update(|v|v.retain(|x|x.id!=p.id));}},Err(e)=>{if !error.is_disposed(){error.set(e);}}}})>{move ||crate::i18n::t("Unblock")}</button></div>}/>
        <Show when=move ||blocks.get().is_empty()><p class="text-sm text-white/50">{move ||crate::i18n::t("No blocked accounts. Block or report someone from their user menu or the Friends list.")}</p></Show></section>
        <section class="settings-card space-y-4"><h3>{move ||crate::i18n::t("Reports")}</h3><div class="flex flex-wrap gap-3">
            <button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|inbox.set(Inbox::Mine)>{move ||crate::i18n::t("My reports")}</button>
            <button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|inbox.set(Inbox::Instance)>{move ||crate::i18n::t("Instance review")}</button>
            <button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|{if let Some(server)=ui.server.get_untracked() && let Some(channel)=ui.selected_channel.get_untracked(){inbox.set(Inbox::Guild {guild_id:server.guild.id,channel_id:channel.id});}else{error.set("Select a server text channel before opening its report inbox".into());}}>{move ||crate::i18n::t("Selected channel review")}</button>
            <button class="settings-secondary" disabled=move ||busy.get() on:click=move |_|load(false)>{move ||crate::i18n::t("Refresh")}</button></div>
        <p class="text-sm text-white/50">{move ||crate::i18n::t("Instance review requires an instance administrator. Channel review requires current membership, history access and Manage messages in that channel. Resolving records a decision; use existing moderation controls for sanctions.")}</p>
        <p role="alert" class="text-red-200">{move ||crate::i18n::message(error.get())}</p>
        <For each=move ||reports.get() key=|r|(r.id,r.revision) children=move |report|view!{<ReportCard ui report reviewing=Signal::derive(move ||inbox.get()!=Inbox::Mine) refresh=Callback::new(move |_|load(false))/>}/>
        <Show when=move ||reports.get().is_empty()><p>{move ||crate::i18n::t("No reports in this page.")}</p></Show>
        <Show when=move ||next.get().is_some()><button disabled=move ||busy.get() on:click=move |_|load(true)>{move ||crate::i18n::t("Next page")}</button></Show>
        </section>
    </div>}
}
#[component]
fn ReportCard(
    ui: Ui,
    report: Report,
    reviewing: Signal<bool>,
    refresh: Callback<()>,
) -> impl IntoView {
    let report = StoredValue::new(report);
    let note = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let resolve = move |state| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let r = report.get_value();
        leptos::task::spawn_local(async move {
            let result = async {
                let guild_revision = if let ReportTarget::GuildMessage {
                    guild_id,
                    channel_id,
                    ..
                } = r.target
                {
                    let account = ui.account.get_untracked().ok_or("Session changed")?;
                    match super::servers::request(
                        ui,
                        PermissionRequest::Preview {
                            guild_id,
                            channel_id: Some(channel_id),
                            account_id: account.id,
                        },
                    )
                    .await?
                    {
                        PermissionResponse::Effective { revision, .. } => Some(revision),
                        _ => return Err("Cannot check server revision".into()),
                    }
                } else {
                    None
                };
                request(
                    ui,
                    &Request::Resolve {
                        id: r.id,
                        revision: r.revision,
                        state,
                        resolution: note.get_untracked(),
                        guild_revision,
                    },
                )
                .await
            }
            .await;
            if busy.is_disposed() {
                return;
            }
            busy.set(false);
            match result {
                Ok(_) => refresh.run(()),
                Err(e) => error.set(e),
            }
        });
    };
    view! {<article class="space-y-3 rounded border border-white/10 p-4"><p class="text-xs text-white/50">{move ||format!("{} · {}",crate::i18n::datetime(report.get_value().created_at),crate::i18n::t(match report.get_value().state {ReportState::Open=>"Awaiting review",ReportState::Resolved=>"Resolved",ReportState::Dismissed=>"Dismissed"}))}</p>
        <p class="text-xs break-all">{move ||thiscord_frontend::localized_format!(crate::i18n::language(),"Report {} · Subject {}",report.get_value().id,report.get_value().subject_id.map(|id|id.to_string()).unwrap_or_else(||crate::i18n::t("Deleted account").into()))}</p>
        <p class="whitespace-pre-wrap break-words">{report.get_value().reason}</p>
        {report.get_value().evidence.map(|e|view!{<blockquote class="space-y-2 border-l-2 border-brand pl-3"><p class="text-xs text-white/50">{move ||thiscord_frontend::localized_format!(crate::i18n::language(),"Submitted message {} · revision {}",e.message_id,e.revision)}</p><p class="whitespace-pre-wrap break-words">{e.content}</p><p class="text-xs">{e.attachments.into_iter().map(|a|a.filename).collect::<Vec<_>>().join(", ")}</p></blockquote>})}
        <p class="whitespace-pre-wrap text-sm">{report.get_value().resolution}</p>
        <Show when=move ||reviewing.get() && report.get_value().state==ReportState::Open><textarea class="w-full rounded bg-white/10 p-2" aria-label=move ||crate::i18n::t("Report resolution") placeholder=move ||crate::i18n::t("Explain the review decision") maxlength="2000" prop:value=move ||note.get() on:input=move |ev|note.set(event_target_value(&ev))/><div class="flex gap-3"><button class="text-brand" disabled=move ||busy.get() on:click=move |_|resolve(ReportState::Resolved)>{move ||crate::i18n::t("Resolve")}</button><button disabled=move ||busy.get() on:click=move |_|resolve(ReportState::Dismissed)>{move ||crate::i18n::t("Dismiss")}</button></div></Show><p role="alert" class="text-red-200">{move ||crate::i18n::message(error.get())}</p>
    </article>}
}
