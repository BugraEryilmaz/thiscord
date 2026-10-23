//! Private transfers reuse the bounded browser transport, never guild URLs.
use super::Ui;
use futures_util::future::{AbortHandle, Abortable};
use leptos::prelude::*;
use thiscord_shared::{chat::*, friends::DIRECT_ATTACHMENTS_PATH, ids::ConversationId};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

#[derive(Clone, Copy)]
pub(super) struct Uploads {
    ui: Ui,
    id: ConversationId,
    pub files: RwSignal<Vec<ChatAttachment>>,
    pub busy: RwSignal<bool>,
    error: RwSignal<String>,
    task: StoredValue<Option<AbortHandle>>,
}
impl Uploads {
    pub fn new(ui: Ui, id: ConversationId) -> Self {
        let value = Self {
            ui,
            id,
            files: RwSignal::new(vec![]),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            task: StoredValue::new(None),
        };
        on_cleanup(move || {
            if let Some(task) = value.task.get_value() {
                task.abort();
            }
        });
        value
    }
    fn add(self, file: web_sys::File) {
        if self.busy.get_untracked() {
            return;
        }
        if let Err(e) = thiscord_frontend::attachments::validate_file(
            file.size() as u64,
            self.files.with_untracked(Vec::len),
        ) {
            self.error.set(e.into());
            return;
        }
        let Some(token) = self.ui.token.get_untracked() else {
            return;
        };
        let (handle, registration) = AbortHandle::new_pair();
        self.task.set_value(Some(handle));
        self.busy.set(true);
        self.error.set(String::new());
        leptos::task::spawn_local(async move {
            let _=Abortable::new(async move {
                let result=async {
                    let _permit=super::chat::AttachmentPermit::acquire(file.size() as u64).await?;
                    let path=format!("{DIRECT_ATTACHMENTS_PATH}?conversation_id={}&filename={}&content_type={}",self.id,js_sys::encode_uri_component(&file.name()),js_sys::encode_uri_component(&file.type_()));
                    let blob=super::chat::attachment_transfer("POST",&path,&token,Some(&file),32768,|_|{}).await?;
                    let text=JsFuture::from(blob.text()).await.map_err(|_|"Invalid upload response")?.as_string().ok_or("Invalid upload response")?;
                    serde_json::from_str::<ChatAttachment>(&text).map_err(|_|"Invalid upload response".into())
                }.await;
                if self.busy.is_disposed(){return;}
                self.busy.set(false);
                if self.ui.token.get_untracked()!=Some(token) || self.ui.friends.selected.get_untracked()!=Some(self.id){return;}
                match result {Ok(a)=>self.files.update(|files|files.push(a)),Err(e)=>self.error.set(e)}
            },registration).await;
        });
    }
}
#[component]
pub(super) fn Composer(uploads: Uploads, locked: Signal<bool>) -> impl IntoView {
    view! {<div class="mb-2 space-y-2 text-sm">
        <label class="inline-block rounded bg-white/10 px-3 py-2">{move ||crate::i18n::t("Attach file (25 MiB)")}
            <input class="block max-w-full text-xs" type="file" disabled=move ||locked.get()||uploads.busy.get()||uploads.files.with(|v|v.len()>=4)||!uploads.ui.friends.connected.get()
                on:change=move |ev|{let input=event_target::<web_sys::HtmlInputElement>(&ev);if let Some(file)=input.files().and_then(|f|f.get(0)){uploads.add(file);}input.set_value("");}/>
        </label>
        <Show when=move ||uploads.busy.get()><span role="status">{move ||crate::i18n::t("Uploading…")}</span></Show>
        <For each=move ||uploads.files.get() key=|a|a.id children=move |a|view!{<div class="flex items-center gap-3"><span>{a.filename}</span><button type="button" disabled=move ||locked.get() class="text-red-200" on:click=move |_|uploads.files.update(|files|files.retain(|f|f.id!=a.id))>{move ||crate::i18n::t("Remove attachment")}</button></div>}/>
        <p role="alert" class="text-red-200">{move ||crate::i18n::message(uploads.error.get())}</p>
    </div>}
}
struct Asset {
    url: String,
    _permit: super::chat::AttachmentPermit,
}
impl Drop for Asset {
    fn drop(&mut self) {
        let _ = web_sys::Url::revoke_object_url(&self.url);
    }
}
#[component]
pub(super) fn Card(ui: Ui, id: ConversationId, attachment: ChatAttachment) -> impl IntoView {
    let attachment = StoredValue::new(attachment);
    let asset = StoredValue::new_local(None::<Asset>);
    let url = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let task = StoredValue::new(None::<AbortHandle>);
    let listener = StoredValue::new_local(None::<crate::account_client::EventListener>);
    let session = StoredValue::new(ui.token.get_untracked());
    let clear = move || {
        if let Some(task) = task.get_value() {
            task.abort();
        }
        asset.set_value(None);
        url.set(None);
        busy.set(false);
    };
    on_cleanup(move || {
        clear();
        listener.set_value(None);
    });
    Effect::new(move |_| {
        if !ui.friends.connected.get()
            || ui.friends.selected.get() != Some(id)
            || ui.token.get() != session.get_value()
        {
            clear();
        }
    });
    if crate::account_client::desktop() {
        leptos::task::spawn_local(async move {
            let result =
                crate::account_client::listen::<ChatDownload>(CHAT_DOWNLOAD_EVENT, move |event| {
                    if !url.is_disposed()
                        && url.get_untracked().as_deref() == Some(&event.url)
                        && !event.success
                    {
                        error.set("Download failed; try again".into());
                    }
                })
                .await;
            if !listener.is_disposed() {
                listener.set_value(result.ok());
            }
        });
    }
    let fetch = move |download: bool| {
        if busy.get_untracked() || !ui.friends.connected.get_untracked() {
            return;
        }
        let Some(token) = ui.token.get_untracked() else {
            return;
        };
        clear();
        busy.set(true);
        error.set(String::new());
        let (handle, registration) = AbortHandle::new_pair();
        task.set_value(Some(handle));
        leptos::task::spawn_local(async move {
            let _ = Abortable::new(
                async move {
                    let a = attachment.get_value();
                    let result = async {
                        let mut permit =
                            super::chat::AttachmentPermit::acquire(a.size_bytes).await?;
                        let blob = super::chat::attachment_transfer(
                            "GET",
                            &format!("{DIRECT_ATTACHMENTS_PATH}/{}", a.id),
                            &token,
                            None,
                            a.size_bytes,
                            |_| {},
                        )
                        .await?;
                        if blob.size() != a.size_bytes as f64 {
                            return Err("Attachment size does not match its metadata".to_string());
                        }
                        let kind = thiscord_frontend::attachments::preview_kind(&a.content_type);
                        let mime = if kind == thiscord_frontend::attachments::PreviewKind::File {
                            "application/octet-stream"
                        } else {
                            &a.content_type
                        };
                        let blob = blob
                            .slice_with_f64_and_f64_and_content_type(0.0, blob.size(), mime)
                            .map_err(|_| "Cannot prepare attachment")?;
                        let object = web_sys::Url::create_object_url_with_blob(&blob)
                            .map_err(|_| "Cannot prepare attachment")?;
                        permit.finished();
                        Ok(Asset {
                            url: object,
                            _permit: permit,
                        })
                    }
                    .await;
                    if busy.is_disposed() {
                        return;
                    }
                    busy.set(false);
                    if ui.token.get_untracked() != Some(token)
                        || !ui.friends.connected.get_untracked()
                        || ui.friends.selected.get_untracked() != Some(id)
                    {
                        return;
                    }
                    match result {
                        Ok(loaded) => {
                            let path = loaded.url.clone();
                            asset.set_value(Some(loaded));
                            url.set(Some(path.clone()));
                            if download {
                                let result = (|| -> Result<(), String> {
                                    let link = document()
                                        .create_element("a")
                                        .map_err(|_| "Cannot download")?
                                        .dyn_into::<web_sys::HtmlAnchorElement>()
                                        .map_err(|_| "Cannot download")?;
                                    link.set_href(&path);
                                    link.set_download(
                                        &thiscord_frontend::attachments::download_name(&a.filename),
                                    );
                                    document()
                                        .body()
                                        .ok_or("Cannot download")?
                                        .append_child(&link)
                                        .map_err(|_| "Cannot download")?;
                                    link.click();
                                    link.remove();
                                    Ok(())
                                })();
                                if let Err(e) = result {
                                    error.set(e);
                                }
                                // WebViews may consume a blob asynchronously. Bound retained bytes;
                                // every later view/download is a fresh authenticated fetch.
                                set_timeout(
                                    move || {
                                        if !url.is_disposed() && url.get_untracked() == Some(path) {
                                            asset.set_value(None);
                                            url.set(None);
                                        }
                                    },
                                    std::time::Duration::from_secs(60),
                                );
                            }
                        }
                        Err(e) => error.set(e),
                    }
                },
                registration,
            )
            .await;
        });
    };
    let kind = thiscord_frontend::attachments::preview_kind(&attachment.get_value().content_type);
    view! {<div class="my-2 max-w-xl space-y-2 rounded border border-white/10 p-3">
        <p class="break-words text-sm">{attachment.get_value().filename}{format!(" · {:.1} MiB",attachment.get_value().size_bytes as f64/1048576.0)}</p>
        <div class="flex gap-3 text-sm"><Show when=move ||kind!=thiscord_frontend::attachments::PreviewKind::File><button type="button" class="text-brand" disabled=move ||busy.get()||!ui.friends.connected.get() on:click=move |_|fetch(false)>{move ||crate::i18n::t("View")}</button></Show>
        <button type="button" class="text-brand" disabled=move ||busy.get()||!ui.friends.connected.get() on:click=move |_|fetch(true)>{move ||crate::i18n::t("Download")}</button>
        <Show when=move ||url.get().is_some()><button type="button" on:click=move |_|clear()>{move ||crate::i18n::t("Close preview")}</button></Show></div>
        {move ||url.get().map(|url|match kind {thiscord_frontend::attachments::PreviewKind::Image=>view!{<img class="max-h-96 max-w-full object-contain" src=url alt=attachment.get_value().filename/>}.into_any(),thiscord_frontend::attachments::PreviewKind::Video=>view!{<video class="max-h-96 max-w-full" src=url controls=true preload="metadata"/>}.into_any(),_=>().into_any()})}
        <Show when=move ||busy.get()><p role="status">{move ||crate::i18n::t("Loading…")}</p></Show><p class="text-sm text-red-200" role="alert">{move ||crate::i18n::message(error.get())}</p>
    </div>}
}
