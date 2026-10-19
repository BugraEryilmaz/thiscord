//! Control-only screen IPC. Native capture/encoding and compressed WebRTC relay;
//! the WebView owns video decoding and presentation.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, State};
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
    pub own_slot: usize,
    pub local_epoch: Arc<AtomicU32>,
    pub local_visible: Arc<AtomicBool>,
    pub video: Arc<TrackLocalStaticRTP>,
    pub audio: Arc<TrackLocalStaticRTP>,
    pub can_publish: bool,
    pub subscriptions: bool,
    pub video_sequence: Arc<AtomicU32>,
    pub audio_sequence: Arc<AtomicU32>,
    pub audio_timestamp: Arc<AtomicU32>,
    pub started: Instant,
    pub force_keyframe: Arc<AtomicBool>,
    pub demand: Arc<thiscord_frontend::screen::adaptation::Demand>,
    pub network_demand: Arc<thiscord_frontend::screen::adaptation::Demand>,
    pub metrics: thiscord_frontend::screen::metrics::Metrics,
}
#[derive(Default)]
struct Inner {
    binding: Option<Binding>,
    local: Option<Arc<Sender>>,
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
#[derive(Default)]
struct Feedback {
    receiver: thiscord_frontend::screen::adaptation::Receiver,
    previous: Diagnostics,
    sampled: Option<Instant>,
    decoder_pressure: bool,
}
struct Viewer {
    feedback: Feedback,
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
pub struct ScreenState(Arc<Mutex<Inner>>, Option<thiscord_shared::voice::MediaKind>);
impl ScreenState {
    pub fn camera() -> Self {
        Self(
            Default::default(),
            Some(thiscord_shared::voice::MediaKind::Camera),
        )
    }
    fn kind(&self) -> thiscord_shared::voice::MediaKind {
        self.1
            .unwrap_or(thiscord_shared::voice::MediaKind::ScreenVideo)
    }
}
pub fn video_state(app: &tauri::AppHandle, kind: thiscord_shared::voice::MediaKind) -> ScreenState {
    if kind == thiscord_shared::voice::MediaKind::Camera {
        app.state::<crate::native_camera::CameraState>()
            .views
            .clone()
    } else {
        app.state::<ScreenState>().inner().clone()
    }
}
impl ScreenState {
    pub fn target(&self, bitrate: u32) {
        if let Ok(inner) = self.0.lock()
            && let Some(binding) = &inner.binding
        {
            if binding.network_demand.bitrate() == 0 && bitrate > 0 {
                binding.force_keyframe.store(true, Ordering::Release);
            }
            binding.network_demand.update(bitrate);
            let own_view = inner.viewers[binding.own_slot].as_ref().filter(|v| {
                v.connection.active() && v.touched.elapsed() < VIEWER_LEASE && v.preview.is_some()
            });
            // A local preview keeps capture alive without making an SFU round trip.
            let budget = own_view.map_or(bitrate, |v| {
                if bitrate == 0 {
                    v.feedback.receiver.bitrate()
                } else {
                    bitrate.min(v.feedback.receiver.bitrate())
                }
            });
            binding.demand.update(budget);
        }
    }
    pub fn subscriptions(&self) -> Vec<Subscription> {
        let Ok(mut inner) = self.0.lock() else {
            return vec![];
        };
        if inner
            .binding
            .as_ref()
            .is_none_or(|b| !b.connection.active())
        {
            return vec![];
        }
        let own_slot = inner.binding.as_ref().map(|b| b.own_slot);
        if let Some(binding) = &inner.binding {
            let live = inner.viewers[binding.own_slot].as_ref().is_some_and(|v| {
                v.connection.active() && v.touched.elapsed() < VIEWER_LEASE && v.preview.is_some()
            });
            if binding.local_visible.swap(live, Ordering::AcqRel) != live && live {
                binding.force_keyframe.store(true, Ordering::Release);
            }
        }
        let mut views = Vec::new();
        for slot in 0..ROOM_CAPACITY {
            let metrics = inner.receivers[slot].clone();
            let owner = inner.owners[slot];
            let Some(viewer) = inner.viewers[slot].as_mut() else {
                continue;
            };
            if !viewer.connection.active()
                || viewer.touched.elapsed() >= VIEWER_LEASE
                || viewer
                    .preview
                    .as_ref()
                    .is_none_or(|p| !p.connected.load(Ordering::Acquire))
                || owner != Some((viewer.watch.owner, viewer.watch.epoch))
            {
                continue;
            }
            let feedback = &mut viewer.feedback;
            if feedback
                .sampled
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
            {
                let stats = metrics.map(|m| m.snapshot()).unwrap_or_default();
                let delta = |name: &str| {
                    stats
                        .counters
                        .get(name)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(feedback.previous.counters.get(name).copied().unwrap_or(0))
                };
                let lost = delta("lost_packets");
                let congested = lost > 2 && lost * 100 > delta("received_packets").max(1)
                    || delta("queue_resets") > 0
                    || delta("expired_frames") > 0
                    || delta("preview_errors") > 0
                    || feedback.decoder_pressure;
                feedback
                    .receiver
                    .sample(congested, delta("received_frames") > 0);
                feedback.decoder_pressure = false;
                feedback.previous = stats;
                feedback.sampled = Some(Instant::now());
            }
            if Some(slot) == own_slot {
                continue;
            }
            views.push(Subscription {
                kind: self.kind(),
                slot,
                owner: viewer.watch.owner,
                epoch: viewer.watch.epoch,
                bitrate: feedback.receiver.bitrate(),
            });
        }
        views
    }

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
        let local = (self.kind() == thiscord_shared::voice::MediaKind::ScreenVideo)
            .then(|| Arc::new(self.receiver(binding.connection.clone(), binding.own_slot)));
        if let Ok(mut inner) = self.0.lock() {
            inner.status.available =
                binding.can_publish && binding.subscriptions && cfg!(target_os = "windows");
            if !cfg!(target_os = "windows") {
                inner.status.message =
                    "Screen publishing is currently available on Windows.".into();
            } else if !binding.subscriptions {
                inner.status.message =
                    "Update the server to enable screen viewing subscriptions.".into();
            } else if !binding.can_publish {
                inner.status.message = "Speak permission is needed to share your screen.".into();
            }
            inner.local = local;
            inner.binding = Some(binding);
        }
    }
    pub fn clear(&self) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(stop) = inner.stop.take() {
                stop.store(true, Ordering::Release);
            }
            inner.binding = None;
            inner.local = None;
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
                    .find(|m| m.slot == slot && m.video_active(self.kind()))
                    .map(|m| (m.account_id, m.video_epoch(self.kind())));
                if let Some(binding) = &inner.binding
                    && slot == binding.own_slot
                {
                    binding
                        .local_epoch
                        .store(owner.map_or(0, |(_, e)| e), Ordering::Release);
                }
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
                if let Err(reason) = thiscord_frontend::screen::validate_parameter_sets(&frame.data)
                {
                    metrics.add("rejected_parameter_sets", 1);
                    metrics.label("last_parameter_set_rejection", &reason);
                    metrics.event(&reason.to_string());
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
pub fn screen_view_keepalive(
    app: tauri::AppHandle,
    watch: Watch,
    visible: bool,
    decoder_pressure: Option<bool>,
) -> bool {
    let state = video_state(&app, watch.kind);
    if decoder_pressure == Some(true)
        && let Ok(mut inner) = state.0.lock()
        && let Some(viewer) = inner.viewers.get_mut(watch.slot).and_then(Option::as_mut)
        && viewer.watch == watch
    {
        viewer.feedback.decoder_pressure = true;
    }
    state.watch(watch, visible)
}
#[tauri::command]
pub async fn screen_view_open(
    app: tauri::AppHandle,
    offer: ViewOffer,
) -> Result<ViewAnswer, String> {
    let state = video_state(&app, offer.watch.kind);
    static NEGOTIATIONS: tokio::sync::Semaphore =
        tokio::sync::Semaphore::const_new(ROOM_CAPACITY * 2);
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
            feedback: Feedback {
                receiver: thiscord_frontend::screen::adaptation::Receiver::with_maximum(
                    if watch.kind == thiscord_shared::voice::MediaKind::Camera {
                        thiscord_shared::camera::MAX_BITRATE
                    } else {
                        MAX_VIEW_BITRATE
                    },
                ),
                ..Default::default()
            },
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
    if let Some(inbox) = &inner.inboxes[watch.slot] {
        inbox.restart_subscription();
    }
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
pub fn screen_view_keyframe(app: tauri::AppHandle, watch: Watch) -> bool {
    let state = video_state(&app, watch.kind);
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
    if let Some(binding) = &inner.binding
        && binding.own_slot == watch.slot
    {
        binding.force_keyframe.store(true, Ordering::Release);
    }
    if let Some(inbox) = &inner.inboxes[watch.slot] {
        inbox.request_keyframe();
        return true;
    }
    false
}
#[tauri::command]
pub fn screen_view_diagnostics(app: tauri::AppHandle, watch: Watch) -> Result<Diagnostics, String> {
    let state = video_state(&app, watch.kind);
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
        .filter(|b| b.connection.active() && b.can_publish && b.subscriptions)
        .ok_or("Join voice with Speak permission before sharing.")?;
    binding.metrics = Default::default();
    binding.demand = Default::default();
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
    let (tx, rx) = mpsc::channel::<thiscord_frontend::screen::Outgoing>(queue_frames);
    let sender = thiscord_frontend::screen::send::Sender {
        local: inner
            .local
            .clone()
            .map(|tx| thiscord_frontend::screen::send::LocalPreview {
                sender: tx,
                epoch: binding.local_epoch.clone(),
                visible: binding.local_visible.clone(),
            }),
        connection: binding.connection.clone(),
        video: binding.video.clone(),
        demand: binding.demand.clone(),
        network_demand: Some(binding.network_demand.clone()),
        force_keyframe: binding.force_keyframe.clone(),
        sequence: binding.video_sequence.clone(),
        metrics: binding.metrics.clone(),
    };
    tauri::async_runtime::spawn(sender.run(stop.clone(), quality, rx));
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
        binding.demand.clone(),
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
            kind: thiscord_shared::voice::MediaKind::ScreenVideo,
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
                feedback: Feedback::default(),
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
            feedback: Feedback::default(),
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
