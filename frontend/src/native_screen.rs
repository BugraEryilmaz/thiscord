//! Control-only screen IPC. Native capture/encoding and compressed WebRTC relay;
//! the WebView owns video decoding and presentation.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::State;
use thiscord_frontend::{
    audio::connection::Connection,
    screen::receive::{Inbox, MAX_AGE, Sender},
};
use thiscord_shared::{
    AccountId,
    screen::*,
    voice::{Participant, ROOM_CAPACITY},
};
use tokio::sync::mpsc;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_rtp::TrackLocalStaticRTP;

#[derive(Clone)]
#[cfg_attr(
    not(target_os = "windows"),
    expect(
        dead_code,
        reason = "Publishing is Windows-only; other desktop platforms receive shares."
    )
)]
pub struct Binding {
    pub connection: Connection,
    pub video: Arc<TrackLocalStaticRTP>,
    pub audio: Arc<TrackLocalStaticRTP>,
    pub can_publish: bool,
    pub video_sequence: Arc<AtomicU32>,
    pub audio_sequence: Arc<AtomicU32>,
    pub audio_timestamp: Arc<AtomicU32>,
    pub started: Instant,
}
#[derive(Default)]
struct Inner {
    binding: Option<Binding>,
    stop: Option<Arc<AtomicBool>>,
    worker: Option<std::thread::JoinHandle<()>>,
    status: Status,
    owners: [Option<(AccountId, u32)>; ROOM_CAPACITY],
    viewers: [Option<Viewer>; ROOM_CAPACITY],
    audio: bool,
}
struct Viewer {
    watch: Watch,
    touched: Instant,
    connection: Connection,
    preview: Option<Arc<thiscord_frontend::screen::preview::Preview>>,
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.connection.close();
    }
}
#[derive(Clone, Default)]
pub struct ScreenState(Arc<Mutex<Inner>>);
impl ScreenState {
    pub fn bind(&self, binding: Binding) {
        self.clear();
        if let Ok(mut inner) = self.0.lock() {
            inner.status.available = binding.can_publish && cfg!(target_os = "windows");
            if !cfg!(target_os = "windows") {
                inner.status.message =
                    "Screen publishing is currently available on Windows.".into();
            } else if !binding.can_publish {
                inner.status.message = "Speak permission is needed to share your screen.".into();
            }
            inner.binding = Some(binding);
        }
    }
    pub fn clear(&self) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(stop) = inner.stop.take() {
                stop.store(true, Ordering::Release);
            }
            inner.binding = None;
            inner.status = Status::default();
            inner.owners = [None; ROOM_CAPACITY];
            inner.viewers = std::array::from_fn(|_| None);
        }
    }
    pub fn sharing(&self) -> (bool, bool) {
        self.0
            .lock()
            .map(|s| (s.status.sharing, s.status.sharing && s.audio))
            .unwrap_or_default()
    }
    pub fn roster(&self, members: &[Participant]) {
        if let Ok(mut inner) = self.0.lock() {
            for slot in 0..ROOM_CAPACITY {
                let owner = members
                    .iter()
                    .find(|m| m.slot == slot && m.sharing_screen)
                    .map(|m| (m.account_id, m.screen_epoch));
                if owner != inner.owners[slot] {
                    inner.viewers[slot] = None;
                    inner.owners[slot] = owner;
                }
            }
        }
    }
    pub(crate) fn finish(&self, stop: &Arc<AtomicBool>, error: Option<String>) {
        stop.store(true, Ordering::Release);
        if let Ok(mut inner) = self.0.lock()
            && inner.stop.as_ref().is_some_and(|s| Arc::ptr_eq(s, stop))
        {
            if error.is_some() || inner.status.sharing {
                inner.status.message = error.unwrap_or_else(|| "Screen sharing stopped".into());
            }
            inner.status.sharing = false;
        }
    }
    pub fn receiver(&self, connection: Connection, slot: usize) -> Sender {
        let (tx, rx) = Inbox::channel();
        let state = self.clone();
        // Assembly is bounded; this worker only forwards compressed H.264.
        // The WebView's real video decoder owns decoding and presentation.
        std::thread::spawn(move || {
            let mut viewer_connection: Option<Connection> = None;
            let mut sequence = 0;
            while let Some(frame) = rx.next() {
                if !connection.active() {
                    break;
                }
                let target = state.0.lock().ok().and_then(|s| {
                    if s.owners[slot].is_none_or(|(_, epoch)| epoch != frame.epoch)
                        || !s
                            .binding
                            .as_ref()
                            .is_some_and(|b| b.connection.accepts(&connection))
                    {
                        return None;
                    }
                    let viewer = s.viewers[slot].as_ref()?;
                    let preview = viewer.preview.as_ref()?;
                    (viewer.connection.active()
                        && viewer.touched.elapsed() < Duration::from_secs(1)
                        && preview.connected.load(Ordering::Acquire))
                    .then(|| (preview.clone(), viewer.connection.clone()))
                });
                let Some((preview, lease)) = target else {
                    rx.resync();
                    continue;
                };
                if !viewer_connection
                    .as_ref()
                    .is_some_and(|c| c.accepts(&lease))
                {
                    viewer_connection = Some(lease.clone());
                    if !frame.reset {
                        rx.resync();
                        continue;
                    }
                }
                if !thiscord_frontend::screen::bounded_parameter_sets(&frame.data) {
                    rx.resync();
                    continue;
                }
                let Ok(packets) = thiscord_frontend::screen::packetize(
                    frame.data,
                    &mut sequence,
                    frame.timestamp,
                ) else {
                    rx.resync();
                    continue;
                };
                let sent = tauri::async_runtime::block_on(async {
                    for packet in packets {
                        if !connection.active()
                            || !lease.active()
                            || frame.arrived.elapsed() > MAX_AGE
                        {
                            return false;
                        }
                        if !matches!(
                            tokio::time::timeout(Duration::from_millis(50), preview.write(packet))
                                .await,
                            Ok(Ok(_))
                        ) {
                            return false;
                        }
                    }
                    true
                });
                if !sent {
                    rx.resync();
                }
            }
            rx.close();
        });
        tx
    }
    fn watch(&self, watch: Watch, visible: bool) -> bool {
        let Ok(mut inner) = self.0.lock() else {
            return false;
        };
        if inner.owners.get(watch.slot) != Some(&Some((watch.owner, watch.epoch))) {
            return false;
        }
        let Some(viewer) = inner.viewers[watch.slot]
            .as_mut()
            .filter(|v| v.watch == watch)
        else {
            return false;
        };
        if visible {
            viewer.touched = Instant::now();
            true
        } else {
            inner.viewers[watch.slot] = None;
            false
        }
    }
}

#[tauri::command]
pub fn screen_view_keepalive(state: State<'_, ScreenState>, watch: Watch, visible: bool) -> bool {
    state.watch(watch, visible)
}
#[tauri::command]
pub async fn screen_view_open(
    state: State<'_, ScreenState>,
    offer: ViewOffer,
) -> Result<ViewAnswer, String> {
    static NEGOTIATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(ROOM_CAPACITY);
    let _permit = NEGOTIATIONS
        .try_acquire()
        .map_err(|_| "Too many screen viewers connecting")?;
    let watch = offer.watch;
    let lease = Connection::default();
    {
        let mut inner = state.0.lock().map_err(|_| "Screen state unavailable")?;
        if !inner
            .binding
            .as_ref()
            .is_some_and(|b| b.connection.active())
            || inner.owners.get(watch.slot) != Some(&Some((watch.owner, watch.epoch)))
        {
            return Err("Screen share is no longer available".into());
        }
        inner.viewers[watch.slot] = Some(Viewer {
            watch,
            touched: Instant::now(),
            connection: lease.clone(),
            preview: None,
        });
    }
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        thiscord_frontend::screen::preview::Preview::answer(&offer.sdp),
    )
    .await;
    let (preview, sdp) = match result {
        Ok(Ok(answer)) => answer,
        _ => {
            state.watch(watch, false);
            return Err("Cannot connect the screen video player".into());
        }
    };
    let mut inner = state.0.lock().map_err(|_| "Screen state unavailable")?;
    let viewer = inner.viewers[watch.slot]
        .as_mut()
        .filter(|v| v.connection.accepts(&lease))
        .ok_or("Screen viewer was closed")?;
    viewer.preview = Some(Arc::new(preview));
    viewer.touched = Instant::now();
    let weak = Arc::downgrade(&state.0);
    tauri::async_runtime::spawn(async move {
        while lease.active() {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let Ok(mut inner) = state.lock() else {
                break;
            };
            if inner.viewers[watch.slot].as_ref().is_some_and(|v| {
                v.connection.accepts(&lease) && v.touched.elapsed() >= Duration::from_secs(1)
            }) {
                inner.viewers[watch.slot] = None;
            }
        }
    });
    Ok(ViewAnswer { sdp })
}

#[tauri::command]
pub fn screen_status(state: State<'_, ScreenState>) -> Result<Status, String> {
    Ok(state
        .0
        .lock()
        .map_err(|_| "Screen state unavailable")?
        .status
        .clone())
}
#[tauri::command]
pub fn screen_stop(state: State<'_, ScreenState>) {
    if let Ok(mut inner) = state.0.lock() {
        if let Some(stop) = &inner.stop {
            stop.store(true, Ordering::Release);
        }
        inner.status.sharing = false;
        inner.status.message = "Screen sharing stopped".into();
    }
}
#[tauri::command]
pub async fn screen_sources() -> Result<Vec<Source>, String> {
    if !cfg!(target_os = "windows") {
        return Err("Screen publishing is currently available on Windows.".into());
    }
    tauri::async_runtime::spawn_blocking(list_sources)
        .await
        .map_err(|_| "Screen scan failed")?
}
#[cfg(not(target_os = "windows"))]
fn list_sources() -> Result<Vec<Source>, String> {
    Err("Screen publishing is currently available on Windows.".into())
}
#[cfg(target_os = "windows")]
fn list_sources() -> Result<Vec<Source>, String> {
    thiscord_frontend::screen::capture::sources()
}

#[tauri::command]
pub async fn screen_start(
    state: State<'_, ScreenState>,
    source: SourceId,
    audio: bool,
    quality: Quality,
) -> Result<(), String> {
    if !quality.valid() {
        return Err("Choose a supported screen resolution and frame rate.".into());
    }
    if !cfg!(target_os = "windows") {
        return Err("Screen publishing is currently available on Windows.".into());
    }
    let mut inner = state.0.lock().map_err(|_| "Screen state unavailable")?;
    if inner.worker.as_ref().is_some_and(|w| !w.is_finished()) {
        return Err("Stop the current share and wait for capture to finish.".into());
    }
    let binding = inner
        .binding
        .clone()
        .filter(|b| b.connection.active() && b.can_publish)
        .ok_or("Join voice with Speak permission before sharing.")?;
    let stop = Arc::new(AtomicBool::new(false));
    inner.stop = Some(stop.clone());
    inner.audio = audio;
    inner.status.message = "Starting screen capture…".into();
    inner.status.encoder = None;
    let state = state.inner().clone();
    let (tx, mut rx) = mpsc::channel::<(Instant, Vec<rtc::rtp::Packet>)>(2);
    let sender_stop = stop.clone();
    let sender_binding = binding.clone();
    let sender_state = state.clone();
    tauri::async_runtime::spawn(async move {
        while let Some((captured_at, packets)) = rx.recv().await {
            if captured_at.elapsed() > Duration::from_millis(500) {
                sender_state.finish(
                    &sender_stop,
                    Some("Screen capture paused; start sharing again.".into()),
                );
                return;
            }
            for mut packet in packets {
                if sender_stop.load(Ordering::Acquire) || !sender_binding.connection.active() {
                    return;
                }
                packet.header.sequence_number = sender_binding
                    .video_sequence
                    .fetch_add(1, Ordering::Relaxed)
                    as u16;
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_millis(100),
                        sender_binding.video.write_rtp(packet)
                    )
                    .await,
                    Ok(Ok(_))
                ) {
                    sender_state.finish(
                        &sender_stop,
                        Some("Screen connection stalled; sharing stopped".into()),
                    );
                    return;
                }
            }
        }
    });
    inner.worker = Some(
        std::thread::Builder::new()
            .name("thiscord-screen-capture".into())
            .spawn(move || {
                let result = capture(&state, &binding, &source, (audio, quality), &stop, tx);
                state.finish(&stop, result.err());
            })
            .map_err(|_| "Cannot start screen capture worker")?,
    );
    Ok(())
}
#[cfg(target_os = "windows")]
fn capture(
    state: &ScreenState,
    binding: &Binding,
    source: &SourceId,
    options: (bool, Quality),
    stop: &Arc<AtomicBool>,
    tx: mpsc::Sender<(Instant, Vec<rtc::rtp::Packet>)>,
) -> Result<(), String> {
    let (audio, quality) = options;
    let audio_job = if audio {
        Some(crate::system_audio::start(
            binding.clone(),
            stop.clone(),
            state.clone(),
        )?)
    } else {
        None
    };
    if let Ok(mut inner) = state.0.lock()
        && !stop.load(Ordering::Acquire)
        && binding.connection.active()
    {
        inner.status.sharing = true;
        inner.status.message = format!(
            "Sharing {}p at {} fps target{}",
            quality.height,
            quality.fps,
            if audio { " with system audio" } else { "" }
        );
    }
    let result = thiscord_frontend::screen::capture::run(
        source,
        quality,
        stop.clone(),
        binding.connection.clone(),
        binding.started,
        tx,
        |encoder| {
            if let Ok(mut inner) = state.0.lock()
                && inner.stop.as_ref().is_some_and(|s| Arc::ptr_eq(s, stop))
            {
                inner.status.encoder = Some(encoder);
            }
        },
    );
    stop.store(true, Ordering::Release);
    if let Some(job) = audio_job {
        let _ = job.join();
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn capture(
    _: &ScreenState,
    _: &Binding,
    _: &SourceId,
    _: (bool, Quality),
    _: &Arc<AtomicBool>,
    _: mpsc::Sender<(Instant, Vec<rtc::rtp::Packet>)>,
) -> Result<(), String> {
    Err("Screen publishing is currently available on Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viewer_replacement_stop_and_roster_revoke_leases() {
        let state = ScreenState::default();
        let owner: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let watch = Watch {
            slot: 0,
            owner,
            epoch: 1,
            viewer: 7,
        };
        let old = Connection::default();
        {
            let mut inner = state.0.lock().unwrap();
            inner.owners[0] = Some((owner, 1));
            inner.viewers[0] = Some(Viewer {
                watch,
                touched: Instant::now(),
                connection: old.clone(),
                preview: None,
            });
        }
        assert!(state.watch(watch, true));
        assert!(!state.watch(Watch { epoch: 2, ..watch }, true));
        let replacement = Connection::default();
        state.0.lock().unwrap().viewers[0] = Some(Viewer {
            watch: Watch { viewer: 8, ..watch },
            touched: Instant::now(),
            connection: replacement.clone(),
            preview: None,
        });
        assert!(!old.active());
        state.watch(watch, false);
        assert!(
            replacement.active(),
            "old cleanup must not close replacement"
        );
        state.roster(&[]);
        assert!(!replacement.active());
    }
    #[test]
    fn late_capture_worker_cannot_stop_or_revive_replacement_share() {
        let state = ScreenState::default();
        let old = Arc::new(AtomicBool::new(false));
        let new = Arc::new(AtomicBool::new(false));
        {
            let mut inner = state.0.lock().unwrap();
            inner.stop = Some(old.clone());
            inner.status.sharing = true;
        }
        state.clear();
        assert!(old.load(Ordering::Acquire));
        assert!(!state.sharing().0);
        {
            let mut inner = state.0.lock().unwrap();
            inner.stop = Some(new.clone());
            inner.status.sharing = true;
        }
        state.finish(&old, Some("late failure".into()));
        assert!(state.sharing().0);
        assert!(!new.load(Ordering::Acquire));
        state.finish(&new, None);
        assert!(!state.sharing().0);
    }
}
