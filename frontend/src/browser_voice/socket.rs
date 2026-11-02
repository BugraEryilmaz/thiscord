//! Voice signaling over the page's WebSocket. A raw `web_sys::WebSocket`
//! (rather than an async sink) lets `pagehide` and teardown send `Leave`
//! synchronously. Frame bodies are never logged.
use std::{cell::RefCell, collections::VecDeque, rc::Rc, task::Waker};
use thiscord_frontend::browser_call::Failure;
use thiscord_shared::voice::{ClientEvent, ClientFrame, VOICE_VERSION};
use wasm_bindgen::{JsCast, prelude::Closure};
use web_sys::{CloseEvent, MessageEvent, WebSocket};

/// Server frames are small and arrive about once per second; a burst beyond
/// this bound means the page is not keeping up and the socket is abandoned.
const INBOX_CAPACITY: usize = 128;

pub(super) enum Incoming {
    Open,
    Text(String),
    Closed(u16),
    Failed,
}
#[derive(Default)]
pub(super) struct Inbox {
    queue: VecDeque<Incoming>,
    waker: Option<Waker>,
    finished: bool,
}
impl Inbox {
    fn push(&mut self, item: Incoming) {
        if self.finished {
            return;
        }
        if matches!(item, Incoming::Closed(_) | Incoming::Failed) {
            self.finished = true;
        } else if self.queue.len() >= INBOX_CAPACITY {
            self.finished = true;
            self.queue.push_back(Incoming::Failed);
            self.wake();
            return;
        }
        self.queue.push_back(item);
        self.wake();
    }
    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}
pub(super) type SharedInbox = Rc<RefCell<Inbox>>;

/// Next event, or `None` after `timeout_ms` without one.
pub(super) async fn next(inbox: &SharedInbox, timeout_ms: u32) -> Option<Incoming> {
    let receive = std::future::poll_fn(|cx| {
        let mut inbox = inbox.borrow_mut();
        match inbox.queue.pop_front() {
            Some(item) => std::task::Poll::Ready(item),
            None => {
                inbox.waker = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    });
    super::within(timeout_ms, receive).await
}

pub(super) struct Socket {
    ws: WebSocket,
    inbox: SharedInbox,
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(CloseEvent)>,
}
impl Socket {
    pub fn open(url: &str) -> Result<Self, Failure> {
        let ws =
            WebSocket::new(url).map_err(|_| Failure::final_("Cannot open the voice connection"))?;
        let inbox: SharedInbox = Rc::default();
        let target = inbox.clone();
        let open = Closure::<dyn FnMut()>::new(move || target.borrow_mut().push(Incoming::Open));
        let target = inbox.clone();
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            // The SFU sends text frames only; anything else is a protocol failure.
            target.borrow_mut().push(match event.data().as_string() {
                Some(text) => Incoming::Text(text),
                None => Incoming::Failed,
            });
        });
        let target = inbox.clone();
        let close = Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
            target.borrow_mut().push(Incoming::Closed(event.code()));
        });
        ws.set_onopen(Some(open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(close.as_ref().unchecked_ref()));
        // Every error event is followed by a close event carrying the code.
        Ok(Self {
            ws,
            inbox,
            _open: open,
            _message: message,
            _close: close,
        })
    }
    pub fn inbox(&self) -> SharedInbox {
        self.inbox.clone()
    }
    pub async fn opened(&self, timeout_ms: u32) -> Result<(), Failure> {
        match next(&self.inbox, timeout_ms).await {
            Some(Incoming::Open) => Ok(()),
            Some(Incoming::Closed(code)) => Err(Failure::closed(code)),
            Some(Incoming::Text(_)) => Err(Failure::final_("Invalid voice event")),
            Some(Incoming::Failed) => Err(Failure::temporary("Cannot reach the voice server")),
            None => Err(Failure::temporary("Voice connection timed out")),
        }
    }
    /// Best effort; a failed send surfaces as a close event.
    pub fn send(&self, event: ClientEvent) {
        if self.ws.ready_state() != WebSocket::OPEN {
            return;
        }
        if let Ok(text) = serde_json::to_string(&ClientFrame {
            version: VOICE_VERSION,
            event,
        }) {
            let _ = self.ws.send_with_str(&text);
        }
    }
    /// Releases the call membership explicitly before closing, so a retry or
    /// a closing page never leaves a member behind until its heartbeat expires.
    pub fn leave(&self) {
        self.send(ClientEvent::Leave {});
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close_with_code(1000);
        let mut inbox = self.inbox.borrow_mut();
        inbox.finished = true;
        inbox.wake();
    }
}
