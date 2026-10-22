//! Authenticated, bounded attachment transfers. No credential-bearing URLs.
use super::Chat;
use crate::account_client as client;
use futures_util::future::{AbortHandle, Abortable};
use leptos::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    task::Poll,
};
use thiscord_frontend::attachments::{
    FetchBudget, PreviewKind, download_name, preview_kind, validate_file,
};
use thiscord_shared::chat::*;
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::JsFuture;

thread_local! {
    static BUDGET: RefCell<FetchBudget> = RefCell::default();
    static DOWNLOADS: RefCell<HashMap<String, Download>> = RefCell::default();
}

struct Download {
    _url: ObjectUrl,
    _asset: Rc<Loaded>,
    chat: Chat,
    generation: u64,
    message: thiscord_shared::MessageId,
    pending: RwSignal<Option<String>>,
}

pub(super) fn clear_downloads(chat: Chat) {
    DOWNLOADS.with_borrow_mut(|v| v.retain(|_, d| d.chat.epoch != chat.epoch));
}
pub(super) fn revoke_message(chat: Chat, message: thiscord_shared::MessageId) {
    DOWNLOADS
        .with_borrow_mut(|v| v.retain(|_, d| d.chat.epoch != chat.epoch || d.message != message));
}
fn finish_download(url: &str, success: Option<bool>) {
    if let Some(d) = DOWNLOADS.with_borrow_mut(|v| v.remove(url)) {
        if !d.pending.is_disposed() {
            d.pending.set(None);
        }
        if current(d.chat, d.generation)
            && let Some(success) = success
        {
            d.chat.status.set(
                if success {
                    crate::i18n::t("Saved to Downloads")
                } else {
                    "Download failed; try again"
                }
                .into(),
            );
        }
    }
}

#[component]
pub(super) fn DownloadHost(chat: Chat) -> impl IntoView {
    let listener = StoredValue::new_local(None::<client::EventListener>);
    on_cleanup(move || {
        listener.set_value(None);
        clear_downloads(chat);
    });
    if client::desktop() {
        leptos::task::spawn_local(async move {
            let result = client::listen::<ChatDownload>(CHAT_DOWNLOAD_EVENT, move |event| {
                finish_download(&event.url, Some(event.success))
            })
            .await;
            if chat.alive() {
                listener.set_value(result.ok());
            }
        });
    }
}

struct Permit {
    bytes: u64,
    active: bool,
}
impl Permit {
    fn new(bytes: u64) -> Result<Self, String> {
        BUDGET
            .with_borrow_mut(|b| b.reserve(bytes))
            .then(|| Self {
                bytes,
                active: true,
            })
            .ok_or_else(|| {
                "Preview capacity reached. Close another preview or scroll away, then retry.".into()
            })
    }
    async fn acquire(bytes: u64) -> Result<Self, String> {
        while BUDGET.with_borrow(FetchBudget::slots_full) {
            gloo_timers::future::TimeoutFuture::new(50).await;
        }
        Self::new(bytes)
    }
    fn finished(&mut self) {
        if self.active {
            BUDGET.with_borrow_mut(FetchBudget::finished);
            self.active = false;
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.finished();
        BUDGET.with_borrow_mut(|b| b.release(self.bytes));
    }
}

struct ObjectUrl(String);
impl ObjectUrl {
    fn new(blob: &web_sys::Blob) -> Result<Self, String> {
        web_sys::Url::create_object_url_with_blob(blob)
            .map(Self)
            .map_err(|_| "Cannot prepare attachment".into())
    }
}
impl Drop for ObjectUrl {
    fn drop(&mut self) {
        let _ = web_sys::Url::revoke_object_url(&self.0);
    }
}
struct Loaded {
    _url: ObjectUrl,
    blob: web_sys::Blob,
    _permit: Permit,
}

/// XHR owns its native cancellation and covers response-body consumption with its
/// timeout. Dropping this guard always aborts the actual browser operation.
struct Transfer {
    xhr: web_sys::XmlHttpRequest,
    upload: web_sys::XmlHttpRequestUpload,
    _end: Closure<dyn FnMut()>,
    _progress: Closure<dyn FnMut(web_sys::ProgressEvent)>,
    _response_limit: Closure<dyn FnMut(web_sys::ProgressEvent)>,
}
impl Drop for Transfer {
    fn drop(&mut self) {
        self.xhr.set_onloadend(None);
        self.xhr.set_onprogress(None);
        self.upload.set_onprogress(None);
        let _ = self.xhr.abort();
    }
}

async fn transfer(
    method: &str,
    path: &str,
    token: &str,
    body: Option<&web_sys::Blob>,
    limit: u64,
    mut progress: impl FnMut(u8) + 'static,
) -> Result<web_sys::Blob, String> {
    let xhr =
        web_sys::XmlHttpRequest::new().map_err(|_| crate::i18n::t("Transfers are unavailable"))?;
    xhr.open(method, &format!("{}{path}", client::api_base()))
        .map_err(|_| crate::i18n::t("Invalid transfer URL"))?;
    xhr.set_request_header("Authorization", &format!("Bearer {token}"))
        .map_err(|_| crate::i18n::t("Cannot authenticate transfer"))?;
    if body.is_some() {
        xhr.set_request_header("Content-Type", "application/octet-stream")
            .map_err(|_| crate::i18n::t("Cannot prepare upload"))?;
    }
    xhr.set_response_type(web_sys::XmlHttpRequestResponseType::Blob);
    xhr.set_timeout(120_000);
    let done = Rc::new(Cell::new(false));
    let waker = Rc::new(RefCell::new(None::<std::task::Waker>));
    let end = {
        let done = done.clone();
        let waker = waker.clone();
        Closure::<dyn FnMut()>::new(move || {
            done.set(true);
            if let Some(w) = waker.borrow_mut().take() {
                w.wake();
            }
        })
    };
    let upload = xhr
        .upload()
        .map_err(|_| crate::i18n::t("Transfers are unavailable"))?;
    let is_upload = body.is_some();
    let check = xhr.clone();
    let progress =
        Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |e: web_sys::ProgressEvent| {
            if !is_upload
                && (e.loaded() > limit as f64
                    || (e.length_computable() && e.total() > limit as f64))
            {
                let _ = check.abort();
            } else if e.length_computable() && e.total() > 0.0 {
                progress(((100.0 * e.loaded() / e.total()).min(100.0)) as u8);
            }
        });
    let check = xhr.clone();
    let response_limit =
        Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |e: web_sys::ProgressEvent| {
            if e.loaded() > limit as f64 || (e.length_computable() && e.total() > limit as f64) {
                let _ = check.abort();
            }
        });
    xhr.set_onloadend(Some(end.as_ref().unchecked_ref()));
    if is_upload {
        upload.set_onprogress(Some(progress.as_ref().unchecked_ref()));
        xhr.set_onprogress(Some(response_limit.as_ref().unchecked_ref()));
    } else {
        xhr.set_onprogress(Some(progress.as_ref().unchecked_ref()));
    }
    let request = Transfer {
        xhr,
        upload,
        _end: end,
        _progress: progress,
        _response_limit: response_limit,
    };
    request
        .xhr
        .send_with_opt_blob(body)
        .map_err(|_| crate::i18n::t("Cannot start transfer"))?;
    futures_util::future::poll_fn(|cx| {
        if done.get() {
            Poll::Ready(())
        } else {
            *waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    })
    .await;
    let status = request.xhr.status().unwrap_or(0);
    if status == 0 {
        return Err(
            "Transfer interrupted, timed out, or exceeded its size limit. Retry when connected."
                .into(),
        );
    }
    let blob: web_sys::Blob = request
        .xhr
        .response()
        .map_err(|_| crate::i18n::t("Missing attachment response"))?
        .dyn_into()
        .map_err(|_| crate::i18n::t("Invalid attachment response"))?;
    if !(200..300).contains(&status) {
        if blob.size() <= 32768.0 {
            let text = JsFuture::from(blob.text())
                .await
                .ok()
                .and_then(|t| t.as_string());
            if let Some(error) =
                text.and_then(|t| serde_json::from_str::<thiscord_shared::ApiError>(&t).ok())
            {
                return Err(crate::i18n::api_error(&error));
            }
        }
        return Err(thiscord_frontend::localized_format!(
            crate::i18n::language(),
            "Attachment request failed ({status})",
            status = status
        ));
    }
    if blob.size() > limit as f64 {
        return Err("Attachment exceeds its size limit".into());
    }
    Ok(blob)
}

fn upload_current(chat: Chat, generation: u64) -> bool {
    chat.alive()
        && chat.access_generation.try_get_untracked() == Some(generation)
        && chat
            .permissions
            .with_untracked(|p| p.contains(&thiscord_shared::permissions::Permission::SendMessages))
}

fn current(chat: Chat, generation: u64) -> bool {
    chat.alive() && chat.attachment_generation.try_get_untracked() == Some(generation)
}

#[derive(Clone)]
struct Queued {
    key: u64,
    filename: String,
    mime: String,
    size: u64,
    preview: Option<String>,
    progress: Option<u8>,
    attachment: Option<ChatAttachment>,
    error: String,
}
#[derive(Clone, Copy)]
pub(super) struct Queue {
    chat: Chat,
    items: RwSignal<Vec<Queued>>,
    files: StoredValue<HashMap<u64, web_sys::File>, LocalStorage>,
    urls: StoredValue<HashMap<u64, ObjectUrl>, LocalStorage>,
    task: StoredValue<Option<AbortHandle>>,
    next: RwSignal<u64>,
}
impl Queue {
    pub(super) fn new(chat: Chat) -> Self {
        let queue = Self {
            chat,
            items: RwSignal::new(vec![]),
            files: StoredValue::new_local(HashMap::new()),
            urls: StoredValue::new_local(HashMap::new()),
            task: StoredValue::new(None),
            next: RwSignal::new(0),
        };
        Effect::new(move |_| {
            chat.access_generation.track();
            queue.clear();
        });
        on_cleanup(move || queue.clear());
        queue
    }
    fn clear(self) {
        if let Some(task) = self.task.get_value() {
            task.abort();
        }
        self.task.set_value(None);
        self.files.update_value(HashMap::clear);
        self.urls.update_value(HashMap::clear);
        self.items.set(vec![]);
    }
    pub(super) fn count(self) -> usize {
        self.items.with(Vec::len)
    }
    pub(super) fn ready(self) -> bool {
        self.items
            .with(|v| v.iter().all(|i| i.attachment.is_some()))
    }
    pub(super) fn take(self) -> Vec<ChatAttachment> {
        let items = self
            .items
            .with_untracked(|v| v.iter().filter_map(|i| i.attachment.clone()).collect());
        self.clear();
        items
    }
    pub(super) fn add(self, files: web_sys::FileList) {
        if !self
            .chat
            .permissions
            .with_untracked(|p| p.contains(&thiscord_shared::permissions::Permission::SendMessages))
        {
            return;
        }
        for file in (0..files.length()).filter_map(|i| files.get(i)) {
            if let Err(error) =
                validate_file(file.size() as u64, self.items.with_untracked(Vec::len))
            {
                self.chat.status.set(error.into());
                break;
            }
            if file.name().len() > MAX_ATTACHMENT_FILENAME_BYTES {
                self.chat
                    .status
                    .set("Filename is too long (maximum 255 UTF-8 bytes)".into());
                continue;
            }
            let key = self.next.get_untracked();
            self.next.update(|v| *v += 1);
            let mime = if file.type_().is_empty() {
                "application/octet-stream".into()
            } else {
                file.type_()
            };
            let preview = if preview_kind(&mime) != PreviewKind::File {
                ObjectUrl::new(&file).ok().map(|url| {
                    let text = url.0.clone();
                    self.urls.update_value(|v| {
                        v.insert(key, url);
                    });
                    text
                })
            } else {
                None
            };
            let item = Queued {
                key,
                filename: file.name(),
                mime,
                size: file.size() as u64,
                preview,
                progress: None,
                attachment: None,
                error: String::new(),
            };
            self.files.update_value(|v| {
                v.insert(key, file);
            });
            self.items.update(|v| v.push(item));
        }
        self.start_next();
    }
    fn remove(self, key: u64) {
        // Cancelling the single upload lets another queued file proceed.
        let active = self
            .items
            .with_untracked(|v| v.iter().any(|i| i.key == key && i.progress.is_some()));
        if active {
            if let Some(task) = self.task.get_value() {
                task.abort();
            }
            self.task.set_value(None);
        }
        self.files.update_value(|v| {
            v.remove(&key);
        });
        self.urls.update_value(|v| {
            v.remove(&key);
        });
        self.items.update(|v| v.retain(|i| i.key != key));
        self.start_next();
    }
    fn start_next(self) {
        if !self
            .chat
            .permissions
            .with_untracked(|p| p.contains(&thiscord_shared::permissions::Permission::SendMessages))
        {
            self.clear();
            return;
        }
        if self.task.get_value().is_some() {
            return;
        }
        let Some(item) = self.items.with_untracked(|v| {
            v.iter()
                .find(|i| i.attachment.is_none() && i.error.is_empty())
                .cloned()
        }) else {
            return;
        };
        let Some(file) = self.files.with_value(|v| v.get(&item.key).cloned()) else {
            return;
        };
        let Some(token) = self.chat.ui.token.get_untracked() else {
            return;
        };
        let generation = self.chat.access_generation.get_untracked();
        let key = item.key;
        self.items.update(|v| {
            if let Some(i) = v.iter_mut().find(|i| i.key == key) {
                i.progress = Some(0);
            }
        });
        let (handle, registration) = AbortHandle::new_pair();
        self.task.set_value(Some(handle));
        leptos::task::spawn_local(async move {
            let _ = Abortable::new(
                async move {
                    let metadata = AttachmentUpload {
                        guild_id: self.chat.guild,
                        channel_id: self.chat.channel,
                        filename: item.filename,
                        content_type: item.mime,
                    };
                    let path = format!(
                        "{ATTACHMENTS_PATH}?guild_id={}&channel_id={}&filename={}&content_type={}",
                        metadata.guild_id,
                        metadata.channel_id,
                        js_sys::encode_uri_component(&metadata.filename),
                        js_sys::encode_uri_component(&metadata.content_type)
                    );
                    let result =
                        transfer("POST", &path, &token, Some(&file), 32768, move |percent| {
                            if upload_current(self.chat, generation) {
                                self.items.update(|v| {
                                    if let Some(i) = v.iter_mut().find(|i| i.key == key) {
                                        i.progress = Some(percent);
                                    }
                                });
                            }
                        })
                        .await;
                    if !upload_current(self.chat, generation) {
                        return;
                    }
                    let result = match result {
                        Ok(blob) => match JsFuture::from(blob.text()).await {
                            Ok(text) => serde_json::from_str::<ChatAttachment>(
                                &text.as_string().unwrap_or_default(),
                            )
                            .map_err(|_| "Invalid upload response".into()),
                            Err(_) => Err("Cannot read upload response".into()),
                        },
                        Err(error) => Err(error),
                    };
                    if !upload_current(self.chat, generation) {
                        return;
                    }
                    self.task.set_value(None);
                    self.items.update(|v| {
                        if let Some(i) = v.iter_mut().find(|i| i.key == key) {
                            i.progress = None;
                            match result {
                                Ok(attachment) => {
                                    i.attachment = Some(attachment);
                                    self.files.update_value(|v| {
                                        v.remove(&key);
                                    });
                                }
                                Err(error) => i.error = error,
                            }
                        }
                    });
                    self.start_next();
                },
                registration,
            )
            .await;
        });
    }
}

#[component]
pub(super) fn Composer(
    queue: Queue,
    editing: RwSignal<Option<thiscord_shared::MessageId>>,
) -> impl IntoView {
    let input = NodeRef::<leptos::html::Input>::new();
    view! {
        <div class="space-y-2">
            <div class="flex items-center gap-3 text-xs text-white/60">
                <input node_ref=input type="file" multiple class="hidden" on:change=move |ev| {
                    let input = event_target::<web_sys::HtmlInputElement>(&ev);
                    if let Some(files) = input.files() { queue.add(files); } input.set_value("");
                }/>
                <button type="button" class="rounded bg-white/10 px-3 py-2 text-sm hover:bg-white/20 disabled:opacity-50"
                    disabled=move || { editing.get().is_some() || queue.count() >= MAX_MESSAGE_ATTACHMENTS || !queue.chat.permissions.with(|p| p.contains(&thiscord_shared::permissions::Permission::SendMessages)) }
                    on:click=move |_| { if let Some(input) = input.get() { input.click(); } }>{move || crate::i18n::t("Attach files")}</button>
                <span>{move || crate::i18n::t("Drop files here · up to 4 files, 25 MiB each")}</span>
            </div>
            <ul class="flex max-h-48 flex-wrap gap-2 overflow-y-auto">
                <For each=move || queue.items.with(|v| v.iter().map(|i| i.key).collect::<Vec<_>>()) key=|key| *key children=move |key| {
                    let item = move || queue.items.with(|v| v.iter().find(|i| i.key == key).cloned());
                    view! { <li class="w-44 space-y-1 rounded-lg bg-black/20 p-2 text-xs">
                        {move || item().and_then(|i| i.preview.map(|url| {
                            if preview_kind(&i.mime) == PreviewKind::Image { view! { <img class="h-16 w-full object-contain" src=url alt=move ||crate::i18n::t("Queued image")/> }.into_any() }
                            else { view! { <video class="h-16 w-full object-contain" src=format!("{url}#t=0.1") muted preload="metadata" aria-label=move || crate::i18n::t("Queued video")/> }.into_any() }
                        }))}
                        <p class="truncate" title=move || item().map(|i| i.filename).unwrap_or_default()>{move || item().map(|i| i.filename).unwrap_or_default()}</p>
                        <p>{move || item().map(|i| format!("{:.1} MiB", i.size as f64 / 1048576.0)).unwrap_or_default()}</p>
                        <p role="status">{move || item().map(|i| if let Some(p) = i.progress { thiscord_frontend::localized_format!(crate::i18n::language(), "Uploading {p}%", p=p) } else if i.attachment.is_some() { crate::i18n::t("Ready to send").into() } else if i.error.is_empty() { crate::i18n::t("Queued").into() } else { crate::i18n::message(i.error) }).unwrap_or_default()}</p>
                        <Show when=move || item().is_some_and(|i| !i.error.is_empty())><button type="button" class="mr-3 text-brand underline" on:click=move |_| {
                            queue.items.update(|v| { if let Some(i) = v.iter_mut().find(|i| i.key == key) { i.error.clear(); } }); queue.start_next();
                        }>{move || crate::i18n::t("Retry upload")}</button></Show>
                        <button type="button" class="text-white/70 underline" on:click=move |_| queue.remove(key)>{move || crate::i18n::t("Remove")}</button>
                    </li> }
                }/>
            </ul>
        </div>
    }
}

#[component]
pub(super) fn Card(
    chat: Chat,
    message: thiscord_shared::MessageId,
    attachment: ChatAttachment,
) -> impl IntoView {
    let kind = preview_kind(&attachment.content_type);
    let attachment = StoredValue::new(attachment);
    let loaded = StoredValue::new_local(None::<Rc<Loaded>>);
    let url = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let task = StoredValue::new(None::<AbortHandle>);
    let alive = RwSignal::new(true);
    let viewer = RwSignal::new(false);
    let download = RwSignal::new(None::<String>);
    let generation = chat.attachment_generation.get_untracked();
    let valid = move || current(chat, generation) && alive.try_get_untracked() == Some(true);
    on_cleanup(move || {
        alive.set(false);
        if let Some(task) = task.get_value() {
            task.abort();
        }
        loaded.set_value(None);
    });
    Effect::new(move |_| {
        if chat.attachment_generation.get() != generation {
            if let Some(task) = task.get_value() {
                task.abort();
            }
            viewer.set(false);
            loaded.set_value(None);
            url.set(None);
        }
    });
    let start_download = move || {
        if !valid() || download.get_untracked().is_some() {
            return;
        }
        let Some(asset) = loaded.get_value() else {
            return;
        };
        let object = match ObjectUrl::new(&asset.blob) {
            Ok(url) => url,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        // Each attempt gets its own URL: late native events/timeouts cannot
        // finish a later download of the same preview.
        let download_url = object.0.clone();
        let result = (|| -> Result<(), String> {
            let link = document()
                .create_element("a")
                .map_err(|_| crate::i18n::t("Cannot create download"))?
                .dyn_into::<web_sys::HtmlAnchorElement>()
                .map_err(|_| crate::i18n::t("Cannot create download"))?;
            link.set_href(&download_url);
            link.set_download(&download_name(&attachment.get_value().filename));
            let body = document()
                .body()
                .ok_or(crate::i18n::t("Cannot start download"))?;
            body.append_child(&link)
                .map_err(|_| crate::i18n::t("Cannot start download"))?;
            download.set(Some(download_url.clone()));
            DOWNLOADS.with_borrow_mut(|v| {
                v.insert(
                    download_url.clone(),
                    Download {
                        _url: object,
                        _asset: asset,
                        chat,
                        generation,
                        message,
                        pending: download,
                    },
                );
            });
            link.click();
            link.remove();
            chat.status.set("Download started".into());
            // Generic files belong only to the in-flight download. Clicking again
            // after completion fetches a fresh, authenticated Blob.
            if kind == PreviewKind::File {
                loaded.set_value(None);
                url.set(None);
            }
            Ok(())
        })();
        if let Err(message) = result {
            error.set(message);
        }
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(if client::desktop() {
                120_000
            } else {
                30_000
            })
            .await;
            finish_download(&download_url, None);
        });
    };
    let fetch = move |download_after: bool| {
        if !valid() || busy.get_untracked() {
            return;
        }
        if url.get_untracked().is_some() {
            if download_after {
                start_download();
            }
            return;
        }
        let a = attachment.get_value();
        let Some(token) = chat.ui.token.get_untracked() else {
            return;
        };
        busy.set(true);
        error.set(String::new());
        let (handle, registration) = AbortHandle::new_pair();
        task.set_value(Some(handle));
        leptos::task::spawn_local(async move {
            let _ = Abortable::new(
                async move {
                    let mut permit = match Permit::acquire(a.size_bytes).await {
                        Ok(p) => p,
                        Err(message) => {
                            if valid() {
                                busy.set(false);
                                error.set(message);
                                task.set_value(None);
                            }
                            return;
                        }
                    };
                    if !valid() {
                        return;
                    }
                    let result = transfer(
                        "GET",
                        &format!("{ATTACHMENTS_PATH}/{}", a.id),
                        &token,
                        None,
                        a.size_bytes,
                        |_| {},
                    )
                    .await;
                    if !valid() {
                        return;
                    }
                    busy.set(false);
                    task.set_value(None);
                    match result {
                        Ok(blob) => {
                            if blob.size() != a.size_bytes as f64 {
                                error.set("Attachment size does not match its metadata".into());
                                return;
                            }
                            // Force a safe MIME for the Blob URL; never render active documents.
                            let mime = if kind == PreviewKind::File {
                                "application/octet-stream"
                            } else {
                                &a.content_type
                            };
                            let Ok(blob) = blob.slice_with_f64_and_f64_and_content_type(
                                0.0,
                                blob.size(),
                                mime,
                            ) else {
                                error.set("Cannot prepare attachment".into());
                                return;
                            };
                            match ObjectUrl::new(&blob) {
                                Ok(object) => {
                                    permit.finished();
                                    url.set(Some(object.0.clone()));
                                    loaded.set_value(Some(Rc::new(Loaded {
                                        _url: object,
                                        blob,
                                        _permit: permit,
                                    })));
                                    if download_after {
                                        start_download();
                                    }
                                }
                                Err(message) => error.set(message),
                            }
                        }
                        Err(message) => error.set(message),
                    }
                },
                registration,
            )
            .await;
        });
    };
    // Only mounted media rows fetch. Generic files never fetch without a click.
    if kind != PreviewKind::File {
        fetch(false);
    }
    view! {
        <div class="my-2 max-w-sm space-y-2 rounded-lg border border-white/10 bg-black/20 p-2">
            <Show when=move || kind != PreviewKind::File && url.get().is_some()>
                <button type="button" class="relative block w-full rounded focus:ring-2 focus:ring-brand" aria-label=move || thiscord_frontend::localized_format!(crate::i18n::language(), "Open {} fullscreen", attachment.get_value().filename)
                    on:click=move |_| { if valid() { viewer.set(true); } }>
                    {move || url.get().map(|url| if kind == PreviewKind::Image {
                        view! { <img class="max-h-64 w-full rounded object-contain" src=url alt=attachment.get_value().filename on:error=move |_| error.set("Image preview unavailable; download the original file".into())/> }.into_any()
                    } else {
                        view! { <Thumbnail url error/> }.into_any()
                    })}
                </button>
            </Show>
            <div class="flex items-center gap-3 text-sm">
                <button type="button" class="min-w-0 flex-1 truncate text-left text-brand underline disabled:opacity-50" disabled=move || busy.get() || download.get().is_some()
                    on:click=move |_| { if valid() { if kind == PreviewKind::File { fetch(true); } else if url.get_untracked().is_some() { viewer.set(true); } else { fetch(false); } } }>
                    {attachment.get_value().filename}
                </button>
                <button type="button" class="shrink-0 rounded bg-white/10 px-2 py-1 hover:bg-white/20 disabled:opacity-50" aria-label=move || crate::i18n::t("Download attachment")
                    disabled=move || busy.get() || download.get().is_some() on:click=move |_| fetch(true)>{move || crate::i18n::t("↓ Download")}</button>
            </div>
            <p class="text-xs text-white/50">{format!("{:.1} MiB", attachment.get_value().size_bytes as f64 / 1048576.0)}</p>
            <Show when=move || busy.get()><p class="text-xs" role="status">{move || crate::i18n::t("Loading attachment…")}</p></Show>
            <Show when=move || !error.get().is_empty()><p class="text-xs text-amber-300" role="status">{move || crate::i18n::message(error.get())}</p><button type="button" class="text-xs text-brand underline" on:click=move |_| { error.set(String::new()); fetch(kind == PreviewKind::File); }>{move || crate::i18n::t("Retry")}</button></Show>
            <Show when=move || viewer.get() && url.get().is_some()>
                <Viewer url=url.get_untracked().unwrap_or_default() kind filename=attachment.get_value().filename download=start_download close=move || viewer.set(false)/>
            </Show>
        </div>
    }
}

#[component]
fn Thumbnail(url: String, error: RwSignal<String>) -> impl IntoView {
    let node = NodeRef::<leptos::html::Video>::new();
    let ready = RwSignal::new(false);
    let failed = RwSignal::new(false);
    let alive = RwSignal::new(true);
    on_cleanup(move || {
        alive.set(false);
        if let Some(v) = node.get_untracked() {
            let _ = v.pause();
            v.remove_attribute("src").ok();
            v.load();
        }
    });
    leptos::task::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(10_000).await;
        if alive.try_get_untracked() == Some(true) && !ready.get_untracked() {
            failed.set(true);
        }
    });
    view! {
        <video node_ref=node class="h-48 w-full rounded bg-black object-contain" src=url muted playsinline preload="auto"
            on:loadedmetadata=move |_| { if let Some(v) = node.get() { v.set_current_time(if v.duration().is_finite() { (v.duration() / 2.0).min(0.1) } else { 0.0 }); } }
            on:seeked=move |_| ready.set(true) on:loadeddata=move |_| ready.set(true)
            on:error=move |_| { failed.set(true); error.set("Video preview unavailable; try playback or download".into()); }/>
        <span class="pointer-events-none absolute inset-0 flex items-center justify-center text-4xl text-white" aria-hidden="true">"▶"</span>
        <Show when=move || failed.get()><span class="absolute bottom-2 left-2 rounded bg-black/80 px-2 text-xs">{move || crate::i18n::t("Video · preview unavailable")}</span></Show>
    }
}

#[component]
fn Viewer(
    url: String,
    kind: PreviewKind,
    filename: String,
    download: impl Fn() + Copy + Send + Sync + 'static,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let video = NodeRef::<leptos::html::Video>::new();
    let error = RwSignal::new(String::new());
    let alive = RwSignal::new(true);
    let previous = StoredValue::new_local(
        document()
            .active_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()),
    );
    Effect::new(move |_| {
        if let Some(d) = dialog.get() {
            let _ = d.show_modal();
        }
    });
    let play = move || {
        if let Some(v) = video.get_untracked() {
            match v.play() {
                Ok(promise) => leptos::task::spawn_local(async move {
                    if JsFuture::from(promise).await.is_err()
                        && alive.try_get_untracked() == Some(true)
                    {
                        error.set(
                            "Playback could not start. Use Play or download the video.".into(),
                        );
                    }
                }),
                Err(_) => error.set("Playback unavailable. Download the original video.".into()),
            }
        }
    };
    Effect::new(move |_| {
        if video.get().is_some() {
            play();
        }
    });
    on_cleanup(move || {
        alive.set(false);
        if let Some(v) = video.get_untracked() {
            let _ = v.pause();
            let _ = v.remove_attribute("src");
            v.load();
        }
        if let Some(d) = dialog.get_untracked() {
            d.close();
        }
        previous.with_value(|v| {
            if let Some(v) = v {
                let _ = v.focus();
            }
        });
    });
    view! {
        <dialog node_ref=dialog class="fixed inset-0 m-0 h-dvh max-h-none w-dvw max-w-none bg-black/95 p-4 text-white backdrop:bg-black/80" aria-label=thiscord_frontend::localized_format!(crate::i18n::language(), "Attachment: {filename}", filename=filename)
            on:cancel=move |e: leptos::ev::Event| { e.prevent_default(); close(); }>
            <div class="flex h-full flex-col gap-3">
                <header class="flex items-center justify-between gap-4"><h2 class="min-w-0 flex-1 truncate">{filename.clone()}</h2><button type="button" class="rounded bg-white/20 px-4 py-2" on:click=move |_| download()>{move || crate::i18n::t("Download")}</button><button autofocus type="button" class="rounded bg-white/20 px-4 py-2 focus:ring-2 focus:ring-brand" on:click=move |_| close()>{move || crate::i18n::t("Close (Esc)")}</button></header>
                {if kind == PreviewKind::Image {
                    view! { <img class="min-h-0 flex-1 object-contain" src=url alt=move ||crate::i18n::t("Full-size attachment") on:error=move |_| error.set("Image could not be displayed".into())/> }.into_any()
                } else {
                    view! { <video node_ref=video class="min-h-0 w-full flex-1 object-contain" src=url autoplay controls playsinline on:error=move |_| error.set("This video format could not be played. Download it to open in another player.".into())/> }.into_any()
                }}
                <Show when=move || !error.get().is_empty()><p role="status">{move || crate::i18n::message(error.get())}</p><Show when=move || kind == PreviewKind::Video><button class="rounded bg-white/20 p-2" on:click=move |_| play()>{move || crate::i18n::t("Play")}</button></Show></Show>
            </div>
        </dialog>
    }
}
