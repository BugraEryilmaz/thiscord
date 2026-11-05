use super::Ui;
use leptos::prelude::*;
use std::{future::Future, pin::Pin, rc::Rc};
use wasm_bindgen::JsCast;

/// Native dialog backdrops target the dialog itself; its padding is still inside.
pub(super) fn dismiss_outside(
    event: leptos::ev::MouseEvent,
    dialog: NodeRef<leptos::html::Dialog>,
    busy: bool,
) {
    if busy || event.target() != event.current_target() {
        return;
    }
    if let Some(dialog) = dialog.get() {
        let bounds = dialog.get_bounding_client_rect();
        let (x, y) = (f64::from(event.client_x()), f64::from(event.client_y()));
        if x < bounds.left() || x > bounds.right() || y < bounds.top() || y > bounds.bottom() {
            dialog.close();
        }
    }
}

/// Opens a native modal dialog. Native modals make the rest of the page inert
/// and close on Escape; this adds `aria-modal` and returns focus to the
/// invoker when closing would otherwise leave it on the page body (WebViews
/// differ here). Callers that move focus elsewhere on close are respected.
pub(super) fn open(dialog: &web_sys::HtmlDialogElement) -> bool {
    if dialog.open() {
        return true;
    }
    let invoker = document()
        .active_element()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|e| !dialog.contains(Some(e)));
    let _ = dialog.set_attribute("aria-modal", "true");
    if dialog.show_modal().is_err() {
        return false;
    }
    if let Some(invoker) = invoker {
        let target = dialog.clone();
        let restore = wasm_bindgen::closure::Closure::once_into_js(move || {
            let lost = document().active_element().is_none_or(|active| {
                document().body().is_some_and(|body| active == *body)
                    || target.contains(Some(&active))
            });
            if lost && invoker.is_connected() {
                let _ = invoker.focus();
            }
        });
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        let _ = dialog.add_event_listener_with_callback_and_add_event_listener_options(
            "close",
            restore.unchecked_ref(),
            &options,
        );
    }
    true
}

/// Whether a native modal dialog is showing (the page behind it is inert).
pub(super) fn modal_open() -> bool {
    let Ok(open) = document().query_selector_all("dialog[open]") else {
        return false;
    };
    (0..open.length()).any(|i| {
        open.get(i)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            .is_some_and(|dialog| dialog.matches(":modal").unwrap_or(true))
    })
}

const FOCUSABLE: &str = "a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), select:not([disabled]), textarea:not([disabled]), summary, [contenteditable=true], [tabindex]";

/// Keeps Tab inside the modal dialog that has focus: from its last control
/// to its first, and Shift+Tab the other way, instead of leaving the page.
pub(super) fn wrap_tab(event: &web_sys::KeyboardEvent) {
    if event.alt_key() || event.ctrl_key() || event.meta_key() {
        return;
    }
    let Some(active) = document().active_element() else {
        return;
    };
    let Some(dialog) = active.closest("dialog[open]").ok().flatten() else {
        return;
    };
    if !dialog.matches(":modal").unwrap_or(true) {
        return;
    }
    let Ok(nodes) = dialog.query_selector_all(FOCUSABLE) else {
        return;
    };
    let items: Vec<web_sys::HtmlElement> = (0..nodes.length())
        .filter_map(|i| nodes.get(i)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|e| {
            let rect = e.get_bounding_client_rect();
            e.tab_index() >= 0 && (rect.width() > 0.0 || rect.height() > 0.0)
        })
        .collect();
    let (Some(first), Some(last)) = (items.first(), items.last()) else {
        event.prevent_default();
        return;
    };
    let wrap = if event.shift_key() {
        (active == **first || active == dialog).then_some(last)
    } else {
        (active == **last).then_some(first)
    };
    if let Some(next) = wrap {
        event.prevent_default();
        let _ = next.focus();
    }
}

type Outcome = Pin<Box<dyn Future<Output = Result<(), String>>>>;

/// The confirmed operation. It receives the optional reason text and reports a
/// (translated) error to keep the dialog open, or `Ok` to close it.
#[derive(Clone)]
pub(super) struct Action(Rc<dyn Fn(String) -> Outcome>);
impl Action {
    pub(super) fn new<F, Fut>(run: F) -> Self
    where
        F: Fn(String) -> Fut + 'static,
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        Self(Rc::new(move |reason| Box::pin(run(reason))))
    }
    /// Adapts a fire-and-forget command that holds `ui.busy` while it runs and
    /// reports its own result elsewhere: the dialog shows progress until it settles.
    pub(super) fn settle(ui: Ui, run: impl Fn() + 'static) -> Self {
        Self::new(move |_| {
            run();
            async move {
                while ui.busy.try_get_untracked().unwrap_or(false) {
                    gloo_timers::future::TimeoutFuture::new(50).await;
                }
                Ok(())
            }
        })
    }
    /// Adapts a component-local command with its own busy flag and error text
    /// (cleared when it starts): the dialog shows progress and that error.
    pub(super) fn tracked(
        run: impl Fn() + 'static,
        busy: RwSignal<bool>,
        error: RwSignal<String>,
    ) -> Self {
        Self::new(move |_| {
            run();
            async move {
                while busy.try_get_untracked().unwrap_or(false) {
                    gloo_timers::future::TimeoutFuture::new(50).await;
                }
                match error.try_get_untracked().filter(|e| !e.is_empty()) {
                    Some(error) => Err(error),
                    None => Ok(()),
                }
            }
        })
    }
}

/// Message for a confirmed action that cannot start because another one is running.
pub(super) fn busy_error() -> String {
    crate::i18n::t("Another action is still in progress. Try again in a moment.").into()
}

/// One destructive-action prompt. Pass already-translated text: the title asks
/// the question, `target` names what is affected and `detail` explains what is
/// lost or kept.
#[derive(Clone)]
pub(super) struct Confirmation {
    title: String,
    target: Option<String>,
    detail: String,
    confirm: String,
    reason: bool,
    action: Action,
}
impl Confirmation {
    pub(super) fn new(
        title: impl Into<String>,
        detail: impl Into<String>,
        confirm: impl Into<String>,
        action: Action,
    ) -> Self {
        Self {
            title: title.into(),
            target: None,
            detail: detail.into(),
            confirm: confirm.into(),
            reason: false,
            action,
        }
    }
    pub(super) fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into()).filter(|t: &String| !t.trim().is_empty());
        self
    }
    /// Offers an optional moderation reason (up to 500 characters).
    pub(super) fn reason(mut self) -> Self {
        self.reason = true;
        self
    }
}

#[derive(Clone, Copy)]
pub(super) struct Confirmations {
    pending: RwSignal<Option<Confirmation>, LocalStorage>,
    return_focus: StoredValue<Option<web_sys::HtmlElement>, LocalStorage>,
    generation: RwSignal<u64>,
    running: RwSignal<bool>,
}
impl Confirmations {
    pub(super) fn new() -> Self {
        Self {
            pending: RwSignal::new_local(None),
            return_focus: StoredValue::new_local(None),
            generation: RwSignal::new(0),
            running: RwSignal::new(false),
        }
    }
}

/// Opens the shared confirmation dialog; focus returns to the active element.
pub(super) fn confirm(ui: Ui, confirmation: Confirmation) {
    let focus = document()
        .active_element()
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok());
    confirm_returning(ui, confirmation, focus);
}

/// Like [`confirm`], for invokers that close before the dialog opens (context
/// menus): focus returns to `focus` instead.
pub(super) fn confirm_returning(
    ui: Ui,
    confirmation: Confirmation,
    focus: Option<web_sys::HtmlElement>,
) {
    let state = ui.confirm;
    // Never replace a prompt whose action is already running.
    if state.running.get_untracked() {
        return;
    }
    state.return_focus.set_value(focus);
    state.generation.update(|g| *g = g.wrapping_add(1));
    state.pending.set(Some(confirmation));
}

/// Accessible confirmation for destructive actions. Cancel is focused first;
/// Escape cancels; Enter only activates the focused button. While the action
/// runs, the dialog cannot be dismissed and shows progress; errors stay inside.
#[component]
pub(super) fn ConfirmHost(ui: Ui) -> impl IntoView {
    let state = ui.confirm;
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let cancel = NodeRef::<leptos::html::Button>::new();
    let reason = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let running = state.running;
    let field = move |get: fn(&Confirmation) -> String| {
        move || {
            state
                .pending
                .with(|p| p.as_ref().map(get).unwrap_or_default())
        }
    };
    Effect::new(move |_| {
        state.generation.track();
        let open = state.pending.with(Option::is_some);
        let Some(element) = dialog.get() else {
            return;
        };
        if open {
            reason.set(String::new());
            error.set(String::new());
            self::open(&element);
            if let Some(cancel) = cancel.get_untracked() {
                let _ = cancel.focus();
            }
        } else if element.open() {
            element.close();
        }
    });
    let close = move || {
        if let Some(element) = dialog.get_untracked() {
            element.close();
        }
    };
    // Signing out or switching accounts dismisses a prompt that is not running.
    let account = Memo::new(move |_| ui.account.with(|a| a.as_ref().map(|a| a.id)));
    Effect::new(move |previous: Option<()>| {
        account.track();
        if previous.is_some() && !running.get_untracked() {
            close();
        }
    });
    let run = move || {
        if running.get_untracked() {
            return;
        }
        let Some(confirmation) = state.pending.get_untracked() else {
            return;
        };
        let generation = state.generation.get_untracked();
        running.set(true);
        error.set(String::new());
        let outcome = (confirmation.action.0)(reason.get_untracked().trim().to_owned());
        leptos::task::spawn_local(async move {
            let result = outcome.await;
            if running.is_disposed() {
                return;
            }
            running.set(false);
            if state.generation.get_untracked() != generation {
                return;
            }
            match result {
                Ok(()) => close(),
                Err(message) => error.set(crate::i18n::message(message)),
            }
        });
    };
    view! {
        <dialog node_ref=dialog class="context-dialog" role="alertdialog" aria-modal="true"
            aria-labelledby="confirm-dialog-title" aria-describedby="confirm-dialog-detail"
            aria-busy=move || running.get().to_string()
            on:click=move |e| dismiss_outside(e, dialog, running.get_untracked())
            on:cancel=move |e: web_sys::Event| if running.get_untracked() { e.prevent_default(); }
            on:close=move |_| {
                state.pending.set(None);
                error.set(String::new());
                if let Some(element) = state.return_focus.try_update_value(Option::take).flatten()
                    && element.is_connected()
                {
                    let _ = element.focus();
                }
            }>
            <h2 id="confirm-dialog-title" class="text-xl font-semibold">{field(|c| c.title.clone())}</h2>
            <Show when=move || state.pending.with(|p| p.as_ref().is_some_and(|c| c.target.is_some()))>
                <p class="mt-3 break-words rounded-lg bg-white/5 px-3 py-2 font-semibold">{field(|c| c.target.clone().unwrap_or_default())}</p>
            </Show>
            <p id="confirm-dialog-detail" class="mt-3 text-sm text-white/70">{field(|c| c.detail.clone())}</p>
            <Show when=move || state.pending.with(|p| p.as_ref().is_some_and(|c| c.reason))>
                <label class="settings-field mt-4"><span>{move || crate::i18n::t("Reason (optional, up to 500 characters)")}</span>
                    <input maxlength="500" disabled=move || running.get() prop:value=move || reason.get() on:input=move |e| reason.set(event_target_value(&e))/></label>
            </Show>
            <p class="mt-3 text-sm text-red-300" role="alert">{move || error.get()}</p>
            <div class="settings-actions mt-6">
                <button node_ref=cancel type="button" class="settings-secondary" disabled=move || running.get() on:click=move |_| close()>{move || crate::i18n::t("Cancel")}</button>
                <button type="button" class="settings-danger" aria-disabled=move || running.get().to_string() on:click=move |_| run()>
                    {move || if running.get() { crate::i18n::t("Working…").to_owned() } else { field(|c| c.confirm.clone())() }}
                </button>
            </div>
        </dialog>
    }
}

/// Shows a clicked profile picture enlarged. Any click or Escape closes it.
#[component]
pub(super) fn AvatarHost(enlarge: crate::avatar::Enlarge) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let field = move |get: fn(&crate::avatar::Enlarged) -> String| {
        move || enlarge.0.with(|e| e.as_ref().map(get).unwrap_or_default())
    };
    Effect::new(move |_| {
        let open = enlarge.0.with(Option::is_some);
        let Some(element) = dialog.get() else {
            return;
        };
        if open {
            self::open(&element);
        } else if element.open() {
            element.close();
        }
    });
    view! {
        <dialog node_ref=dialog class="avatar-viewer" aria-label=field(|e| e.name.clone())
            on:close=move |_| enlarge.0.set(None) on:click=move |_| enlarge.0.set(None)>
            <Show when=move || enlarge.0.with(Option::is_some)>
                <img class="avatar-viewer-image" src=field(|e| e.url.clone()) alt=field(|e| e.name.clone()) draggable="false" referrerpolicy="no-referrer"/>
                <p class="avatar-viewer-name">{field(|e| e.name.clone())}</p>
            </Show>
        </dialog>
    }
}
