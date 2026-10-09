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
const VIEWER_LEASE: Duration = Duration::from_secs(3);
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
    pub force_keyframe: Arc<AtomicBool>,
    pub metrics: thiscord_frontend::screen::metrics::Metrics,
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
    server: std::collections::BTreeMap<String, u64>,
    network: std::collections::BTreeMap<String, String>,
    inboxes: [Option<Inbox>; ROOM_CAPACITY],
    receivers: [Option<thiscord_frontend::screen::metrics::Metrics>; ROOM_CAPACITY],
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
    pub fn network_diagnostics(
        &self,
        connection: &Connection,
        labels: std::collections::BTreeMap<String, String>,
    ) {
        if let Ok(mut inner) = self.0.lock()
            && inner
                .binding
                .as_ref()
                .is_some_and(|b| b.connection.accepts(connection))
        {
            inner.network = labels;
        }
    }
    pub fn watching(&self, slot: usize, epoch: u32) -> bool {
        self.0.lock().ok().is_some_and(|s| {
            s.viewers
                .get(slot)
                .and_then(Option::as_ref)
                .is_some_and(|v| {
                    v.watch.epoch == epoch
                        && v.connection.active()
                        && v.touched.elapsed() < VIEWER_LEASE
                })
        })
    }
    pub fn server_diagnostics(&self, counters: std::collections::BTreeMap<String, u64>) {
        if let Ok(mut s) = self.0.lock() {
            s.server = counters;
        }
    }

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
            inner.server.clear();
            inner.network.clear();
            inner.receivers = Default::default();
            for inbox in inner.inboxes.iter_mut().filter_map(Option::take) {
                inbox.close();
            }
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
        if let Ok(mut inner) = self.0.lock() {
            inner.receivers[slot] = Some(rx.metrics());
            if let Some(old) = inner.inboxes[slot].replace(rx.clone()) {
                old.close();
            }
        }
        let metrics = rx.metrics();
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
                        && viewer.touched.elapsed() < VIEWER_LEASE
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
                let submit = Instant::now();
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
                metrics.time("preview_submit", submit.elapsed());
                if sent {
                    metrics.add("preview_frames", 1);
                }
                if !sent {
                    metrics.add("preview_errors", 1);
                    metrics.event("local WebRTC write timed out or frame expired");
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
        if let Some(metrics) = &inner.receivers[watch.slot] {
            metrics.add("viewer_connections", 1);
            metrics.event("viewer connection opened");
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
                v.connection.accepts(&lease) && v.touched.elapsed() >= VIEWER_LEASE
            }) {
                if let Some(metrics) = &inner.receivers[watch.slot] {
                    metrics.add("viewer_expirations", 1);
                    metrics.event("viewer heartbeat expired");
                }
                inner.viewers[watch.slot] = None;
            }
        }
    });
    Ok(ViewAnswer { sdp })
}

#[tauri::command]
pub fn screen_view_keyframe(state: State<'_, ScreenState>, watch: Watch) -> bool {
    let Ok(inner) = state.0.lock() else {
        return false;
    };
    if inner
        .viewers
        .get(watch.slot)
        .and_then(Option::as_ref)
        .is_none_or(|v| {
            v.watch != watch || !v.connection.active() || v.touched.elapsed() >= VIEWER_LEASE
        })
    {
        return false;
    }
    if let Some(inbox) = &inner.inboxes[watch.slot] {
        inbox.request_keyframe();
        return true;
    }
    false
}
#[tauri::command]
pub fn screen_view_diagnostics(
    state: State<'_, ScreenState>,
    watch: Watch,
) -> Result<Diagnostics, String> {
    let inner = state.0.lock().map_err(|_| "Screen state unavailable")?;
    if inner.owners.get(watch.slot) != Some(&Some((watch.owner, watch.epoch)))
        || !inner
            .viewers
            .get(watch.slot)
            .and_then(Option::as_ref)
            .is_some_and(|v| v.watch == watch)
    {
        return Err("Screen viewer no longer active".into());
    }
    let mut snapshot = inner.receivers[watch.slot]
        .as_ref()
        .map(|m| m.snapshot())
        .unwrap_or_default();
    snapshot.counters.extend(inner.server.clone());
    snapshot.labels.extend(inner.network.clone());
    Ok(snapshot)
}
#[tauri::command]
pub fn screen_status(state: State<'_, ScreenState>) -> Result<Status, String> {
    let inner = state.0.lock().map_err(|_| "Screen state unavailable")?;
    let mut status = inner.status.clone();
    if let Some(binding) = &inner.binding {
        status.diagnostics = binding.metrics.snapshot();
    }
    status.diagnostics.counters.extend(inner.server.clone());
    status.diagnostics.labels.extend(inner.network.clone());
    Ok(status)
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
    let mut binding = inner
        .binding
        .clone()
        .filter(|b| b.connection.active() && b.can_publish)
        .ok_or("Join voice with Speak permission before sharing.")?;
    binding.metrics = Default::default();
    binding.force_keyframe.store(true, Ordering::Release);
    inner.binding = Some(binding.clone());
    let stop = Arc::new(AtomicBool::new(false));
    inner.stop = Some(stop.clone());
    inner.audio = audio;
    inner.status.message = "Starting screen capture…".into();
    inner.status.encoder = None;
    let state = state.inner().clone();
    let queue_frames = thiscord_frontend::screen::sender_queue_frames(quality.fps);
    binding.metrics.label("sender_queue_capacity", queue_frames);
    binding.metrics.label("sender_queue_budget_ms", 200);
    let (tx, mut rx) = mpsc::channel::<thiscord_frontend::screen::Outgoing>(queue_frames);
    let sender_stop = stop.clone();
    let sender_binding = binding.clone();
    tauri::async_runtime::spawn(async move {
        let mut pacer = thiscord_frontend::screen::pacing::Pacer::new(
            thiscord_frontend::screen::pacing::transport_bitrate(quality.bitrate()),
        );
        let mut waiting = false;
        'frames: while let Some(mut frame) = rx.recv().await {
            let captured_at = frame.captured_at;
            sender_binding
                .metrics
                .time("sender_queue_wait", frame.enqueued_at.elapsed());
            let stale = captured_at.elapsed() > thiscord_frontend::screen::SENDER_QUEUE_AGE;
            if stale || (waiting && !frame.keyframe) {
                sender_binding.metrics.add(
                    if stale {
                        "sender_stale"
                    } else {
                        "sender_dependent"
                    },
                    1,
                );
                sender_binding.metrics.add("encode_dropped", 1);
                sender_binding
                    .metrics
                    .event("paced sender dropped stale or dependent frame");
                if !waiting || frame.keyframe {
                    sender_binding.force_keyframe.store(true, Ordering::Release);
                }
                waiting = true;
                continue;
            }
            let send_started = Instant::now();
            sender_binding
                .metrics
                .peak("sender_peak_frame_packets", frame.packets.len() as u64);
            for mut packet in frame.packets {
                let bytes = packet.payload.len() + 64;
                let at = pacer.reserve(Instant::now(), bytes);
                if at > Instant::now() {
                    let sleep_started = Instant::now();
                    sender_binding.metrics.add("pacing_sleeps", 1);
                    tokio::time::sleep_until(at.into()).await;
                    sender_binding
                        .metrics
                        .time("pacing_wait", sleep_started.elapsed());
                    sender_binding.metrics.time(
                        "pacing_lateness",
                        Instant::now().saturating_duration_since(at),
                    );
                }
                if sender_stop.load(Ordering::Acquire) || !sender_binding.connection.active() {
                    return;
                }
                if captured_at.elapsed() > Duration::from_millis(350) {
                    waiting = true;
                    sender_binding.force_keyframe.store(true, Ordering::Release);
                    sender_binding.metrics.add("encode_dropped", 1);
                    sender_binding.metrics.add("sender_stale", 1);
                    sender_binding
                        .metrics
                        .event("paced sender dropped stale or dependent frame");
                    continue 'frames;
                }
                packet.header.sequence_number = sender_binding
                    .video_sequence
                    .fetch_add(1, Ordering::Relaxed)
                    as u16;
                let write_started = Instant::now();
                let result = tokio::time::timeout(
                    Duration::from_millis(100),
                    sender_binding.video.write_rtp(packet),
                )
                .await;
                sender_binding
                    .metrics
                    .time("rtp_write", write_started.elapsed());
                if !matches!(result, Ok(Ok(_))) {
                    sender_binding.metrics.add(
                        if result.is_err() {
                            "send_timeouts"
                        } else {
                            "send_errors"
                        },
                        1,
                    );
                    waiting = true;
                    sender_binding.force_keyframe.store(true, Ordering::Release);
                    sender_binding.metrics.add("encode_dropped", 1);
                    sender_binding
                        .metrics
                        .event("paced sender dropped stale or dependent frame");
                    continue 'frames;
                }
                sender_binding.metrics.add("sent_packets", 1);
                sender_binding.metrics.add("sent_bytes", bytes as u64);
                frame.budget.sent(bytes);
            }
            waiting = false;
            sender_binding
                .metrics
                .time("frame_send", send_started.elapsed());
            sender_binding.metrics.add("sent_frames", 1);
            sender_binding
                .metrics
                .time("send_age", captured_at.elapsed());
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
    tx: mpsc::Sender<thiscord_frontend::screen::Outgoing>,
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
        binding.metrics.clone(),
        binding.force_keyframe.clone(),
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
    _: mpsc::Sender<thiscord_frontend::screen::Outgoing>,
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
        state.0.lock().unwrap().viewers[0].as_mut().unwrap().touched =
            Instant::now() - Duration::from_millis(1500);
        assert!(
            state.watching(0, 1),
            "short UI stalls should not reconnect the video transport"
        );
        state.0.lock().unwrap().viewers[0].as_mut().unwrap().touched =
            Instant::now() - VIEWER_LEASE;
        assert!(!state.watching(0, 1));
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
