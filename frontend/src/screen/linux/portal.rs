//! xdg-desktop-portal ScreenCast session. The system dialog owns source
//! selection (required on Wayland, also used on X11 portals). A dedicated
//! thread owns the D-Bus connection for the whole share; closing the session
//! (user stop in the shell, logout) is reported to the encoder worker.
use ashpd::desktop::{
    PersistMode, Session,
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use std::{
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

pub const UNAVAILABLE: &str = "Screen sharing needs xdg-desktop-portal with a ScreenCast backend (GNOME, KDE, wlroots or Hyprland portal).";
const CANCELLED: &str = "Screen selection was cancelled.";

/// A private session-bus connection on the calling runtime. ashpd's default
/// `new()` caches one process-wide connection whose driver tasks would die with
/// the short-lived runtime that created it. Dropping this connection when its
/// runtime ends also makes the portal close any session or dialog it owned.
async fn screencast() -> Result<Screencast, String> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|_| UNAVAILABLE.to_owned())?;
    Screencast::with_connection(connection)
        .await
        .map_err(|_| UNAVAILABLE.to_owned())
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start the screen portal client.".to_owned())
}

/// Source kinds the portal offers: (screens, windows).
pub fn available() -> Result<(bool, bool), String> {
    runtime()?.block_on(async {
        let proxy = screencast().await?;
        let types = proxy
            .available_source_types()
            .await
            .map_err(|_| UNAVAILABLE.to_owned())?;
        Ok((
            types.contains(SourceType::Monitor),
            types.contains(SourceType::Window),
        ))
    })
}

fn describe(error: ashpd::Error) -> String {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => CANCELLED.into(),
        ashpd::Error::Portal(ashpd::PortalError::Cancelled(_)) => CANCELLED.into(),
        _ => "The screen sharing portal failed to start a capture session.".into(),
    }
}

pub struct Portal {
    fd: OwnedFd,
    node: u32,
    closed: Arc<AtomicBool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
type Opened = Result<(OwnedFd, u32), String>;
impl Portal {
    /// Shows the system picker. `stop` cancels a pending dialog.
    pub fn open(window: bool, stop: &AtomicBool) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel::<Opened>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let closed = Arc::new(AtomicBool::new(false));
        let session_closed = closed.clone();
        let runtime = runtime()?;
        let thread = std::thread::Builder::new()
            .name("thiscord-screencast-portal".into())
            .spawn(move || {
                runtime.block_on(session(window, ready_tx, stop_rx, session_closed));
            })
            .map_err(|_| "Cannot start the screen portal client.")?;
        let mut stop_tx = Some(stop_tx);
        let mut thread = Some(thread);
        let mut abandon = || {
            if let Some(stop) = stop_tx.take() {
                let _ = stop.send(());
            }
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        };
        let result = loop {
            match ready_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                // The user pressed Stop while the system dialog was open.
                Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Acquire) => {
                    break Err(CANCELLED.into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(UNAVAILABLE.into()),
            }
        };
        match result {
            Ok((fd, node)) => Ok(Self {
                fd,
                node,
                closed,
                stop: stop_tx.take(),
                thread: thread.take(),
            }),
            Err(error) => {
                abandon();
                Err(error)
            }
        }
    }
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
    pub fn node(&self) -> u32 {
        self.node
    }
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}
impl Drop for Portal {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn session(
    window: bool,
    ready: mpsc::Sender<Opened>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    closed: Arc<AtomicBool>,
) {
    let start = async {
        let proxy = screencast().await?;
        let cursor = proxy
            .available_cursor_modes()
            .await
            .ok()
            .filter(|modes| modes.contains(CursorMode::Embedded))
            .map(|_| CursorMode::Embedded);
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(describe)?;
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor)
                    .set_sources(ashpd::enumflags2::BitFlags::from(if window {
                        SourceType::Window
                    } else {
                        SourceType::Monitor
                    }))
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .map_err(describe)?
            .response()
            .map_err(describe)?;
        let streams = proxy
            .start(&session, None, Default::default())
            .await
            .map_err(describe)?
            .response()
            .map_err(describe)?;
        let node = streams
            .streams()
            .first()
            .map(|s| s.pipe_wire_node_id())
            .ok_or(CANCELLED)?;
        let fd = proxy
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(describe)?;
        Ok::<_, String>((proxy, session, fd, node))
    };
    let opened = tokio::select! {
        opened = start => opened,
        _ = &mut stop => return,
    };
    let (_proxy, session, fd, node): (Screencast, Session<Screencast>, OwnedFd, u32) = match opened
    {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let signal = session.receive_closed().await;
    if ready.send(Ok((fd, node))).is_err() {
        let _ = session.close().await;
        return;
    }
    match signal {
        Ok(signal) => {
            let mut signal = std::pin::pin!(signal);
            tokio::select! {
                _ = std::future::poll_fn(|cx| {
                    use ashpd::zbus::export::futures_core::Stream;
                    signal.as_mut().poll_next(cx)
                }) => closed.store(true, Ordering::Release),
                _ = &mut stop => {}
            }
        }
        Err(_) => {
            let _ = (&mut stop).await;
        }
    }
    let _ = session.close().await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn repeated_portal_queries_do_not_reuse_a_dead_connection() {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            eprintln!("no session bus; skipping");
            return;
        }
        // Each query runs on its own short-lived runtime. A connection cached
        // from the first runtime would hang or fail once that runtime is gone.
        let query = || {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(super::available());
            });
            rx.recv_timeout(std::time::Duration::from_secs(20))
                .expect("portal query hung")
        };
        let first = query();
        let second = query();
        eprintln!("portal: {first:?} then {second:?}");
        assert_eq!(first, second);
    }
}
