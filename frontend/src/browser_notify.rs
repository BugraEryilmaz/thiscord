//! System notifications for the browser web client (the Web `Notification`
//! API). They mirror the desktop's notification windows for alerts that would
//! show a banner, but only while the tab is hidden or unfocused; in-app
//! banners and sounds are unchanged. Permission is requested only from an
//! explicit click in Settings → Notifications, and the user can turn them off
//! for this browser. Notifications carry the alert's source title, never
//! message text or tokens, and close when the tab regains focus.
use js_sys::Reflect;
use std::{cell::RefCell, collections::VecDeque};
use thiscord_frontend::web_notifications::{MAX_OPEN, Permission, lifetime_ms};
use thiscord_shared::{ids::CallId, notifications::Alert};
use wasm_bindgen::{JsCast, prelude::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Notification, NotificationOptions};

const ENABLED_KEY: &str = "thiscord.browser-notifications.v1";

struct Open {
    id: String,
    call: Option<CallId>,
    notification: Notification,
}
thread_local! {
    static OPEN: RefCell<VecDeque<Open>> = const { RefCell::new(VecDeque::new()) };
    static LISTENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn supported() -> bool {
    Reflect::has(&js_sys::global(), &"Notification".into()).unwrap_or(false)
}
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
/// The browser's current permission for this site.
pub fn permission() -> Permission {
    if !supported() {
        return Permission::Unsupported;
    }
    let value = Reflect::get(&js_sys::global(), &"Notification".into())
        .and_then(|class| Reflect::get(&class, &"permission".into()))
        .ok()
        .and_then(|value| value.as_string());
    Permission::parse(Some(value.as_deref().unwrap_or("default")))
}
/// The user's choice for this browser; on unless turned off.
pub fn enabled() -> bool {
    storage()
        .and_then(|s| s.get_item(ENABLED_KEY).ok().flatten())
        .is_none_or(|value| value != "0")
}
pub fn set_enabled(enabled: bool) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(ENABLED_KEY, if enabled { "1" } else { "0" });
    }
    if !enabled {
        close_all();
    }
}
/// Asks the browser for permission. Call synchronously from a click handler:
/// the request starts before this returns, inside the user gesture.
pub fn request() -> impl std::future::Future<Output = Permission> {
    let pending = supported()
        .then(Notification::request_permission)
        .and_then(Result::ok);
    async move {
        if let Some(promise) = pending {
            let _ = JsFuture::from(promise).await;
        }
        let granted = permission();
        if granted == Permission::Granted {
            set_enabled(true);
        }
        granted
    }
}
/// The tab is hidden or another window or app has focus.
pub fn away() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_none_or(|d| d.hidden() || !d.has_focus().unwrap_or(false))
}

/// Shows a notification for `alert`. Clicking it focuses this tab and runs
/// `open` (the banner's open action).
pub fn show(alert: &Alert, body: &str, open: impl FnOnce() + 'static) {
    install();
    let (title, body) = thiscord_frontend::web_notifications::text(alert, body);
    let options = NotificationOptions::new();
    options.set_body(&body);
    // Replaces an earlier notification for the same alert (call replays).
    options.set_tag(&alert.id);
    // The page plays the event's own sound; avoid a second system sound.
    options.set_silent(Some(true));
    let Ok(notification) = Notification::new_with_options(&title, &options) else {
        return;
    };
    let id = alert.id.clone();
    let click_id = id.clone();
    let click = Closure::once_into_js(move || {
        if let Some(window) = web_sys::window() {
            let _ = window.focus();
        }
        close(&click_id);
        open();
    });
    notification.set_onclick(Some(click.unchecked_ref()));
    OPEN.with(|open| {
        let mut open = open.borrow_mut();
        if let Some(index) = open.iter().position(|o| o.id == id)
            && let Some(previous) = open.remove(index)
        {
            previous.notification.close();
        }
        open.push_back(Open {
            id: id.clone(),
            call: alert.call_id,
            notification,
        });
        while open.len() > MAX_OPEN {
            if let Some(oldest) = open.pop_front() {
                oldest.notification.close();
            }
        }
    });
    let lifetime = lifetime_ms(alert.kind);
    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(lifetime).await;
        close(&id);
    });
}
pub fn close(id: &str) {
    OPEN.with(|open| {
        open.borrow_mut().retain(|o| {
            let keep = o.id != id;
            if !keep {
                o.notification.set_onclick(None);
                o.notification.close();
            }
            keep
        })
    });
}
/// Closes call notifications that are no longer ringing.
pub fn retain_calls(ringing: &[CallId]) {
    OPEN.with(|open| {
        open.borrow_mut().retain(|o| {
            let keep = o.call.is_none_or(|call| ringing.contains(&call));
            if !keep {
                o.notification.close();
            }
            keep
        })
    });
}
pub fn close_all() {
    OPEN.with(|open| {
        for o in open.borrow_mut().drain(..) {
            o.notification.set_onclick(None);
            o.notification.close();
        }
    });
}
/// Returning to the tab shows in-app banners; close the system copies.
fn install() {
    if LISTENING.with(|l| l.replace(true)) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let focus = Closure::<dyn FnMut()>::new(close_all);
    let _ = window.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref());
    focus.forget();
}
