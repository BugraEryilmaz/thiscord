//! Main-WebView counters. No identities, messages, URLs or interaction history.
use std::cell::RefCell;
use thiscord_shared::performance::{UI_TIMINGS, UiPerformance};

thread_local! { static COUNTERS: RefCell<UiPerformance> = RefCell::new(UiPerformance::default()); }

pub fn snapshot() -> UiPerformance {
    COUNTERS.with(|c| c.borrow().clone())
}
fn now() -> Option<f64> {
    web_sys::window()?.performance().map(|p| p.now())
}
fn observe(slot: usize, us: u64) {
    COUNTERS.with(|c| {
        let mut c = c.borrow_mut();
        let t = &mut c.timings[slot];
        t.calls = t.calls.saturating_add(1);
        t.total_us = t.total_us.saturating_add(us);
        t.max_us = t.max_us.max(us);
        t.over_budget = t
            .over_budget
            .saturating_add(u64::from(us > UI_TIMINGS[slot].1));
    });
}
pub struct Timer {
    slot: usize,
    start: Option<f64>,
}
pub fn start(slot: usize) -> Timer {
    Timer { slot, start: now() }
}
impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(elapsed) = self.start.zip(now()).map(|(a, b)| b - a) {
            observe(self.slot, (elapsed.max(0.0) * 1000.0) as u64);
        }
    }
}

/// One task for the main WebView lifetime, never overlays. Desktop snapshots stay
/// in process memory and are exposed only through the existing admin relay.
pub fn install() {
    use std::{cell::Cell, rc::Rc};
    use wasm_bindgen::{JsCast, closure::Closure};
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let visibility = Rc::new(Cell::new(0_u64));
    let changed = visibility.clone();
    let listener = Closure::<dyn FnMut()>::new(move || changed.set(changed.get().wrapping_add(1)));
    if document
        .add_event_listener_with_callback("visibilitychange", listener.as_ref().unchecked_ref())
        .is_err()
    {
        return;
    }
    // Installed exactly once; retained for the lifetime of this main WebView.
    listener.forget();
    leptos::task::spawn_local(async move {
        let mut ticks = 0_u64;
        loop {
            let before = now();
            let epoch = visibility.get();
            let visible = !document.hidden();
            gloo_timers::future::TimeoutFuture::new(1000).await;
            if visible
                && !document.hidden()
                && epoch == visibility.get()
                && let Some(elapsed) = before.zip(now()).map(|(a, b)| b - a)
            {
                observe(3, ((elapsed - 1000.0).max(0.0) * 1000.0) as u64);
            }
            ticks = ticks.wrapping_add(1);
            if ticks.is_multiple_of(10) && crate::account_client::desktop() {
                let _: Result<(), _> = crate::account_client::native(
                    "performance_report",
                    serde_json::json!({"measurements":snapshot()}),
                )
                .await;
            }
        }
    });
}
