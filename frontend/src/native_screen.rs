//! Control-only screen IPC. Capture, codecs and media stay on native workers.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, State};
use thiscord_frontend::{audio::connection::Connection, screen::Assembler};
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
    frames: [Option<(Instant, Vec<u8>)>; ROOM_CAPACITY],
    audio: bool,
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
            inner.frames = std::array::from_fn(|_| None);
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
                    inner.frames[slot] = None;
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
    pub fn decoder(
        &self,
        connection: Connection,
        slot: usize,
    ) -> std::sync::mpsc::SyncSender<rtc::rtp::Packet> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<rtc::rtp::Packet>(4096);
        let state = self.clone();
        std::thread::spawn(move || {
            use openh264::formats::YUVSource;
            let mut owner = None;
            let mut decoder = None;
            let mut assembler = Assembler::default();
            while let Ok(packet) = rx.recv() {
                if !connection.active() {
                    break;
                }
                let current = state.0.lock().ok().and_then(|s| s.owners[slot]);
                if current != owner {
                    owner = current;
                    decoder = openh264::decoder::Decoder::new().ok();
                    assembler = Assembler::default();
                }
                if owner.is_none()
                    || packet.header.csrc.first().copied() != owner.map(|(_, epoch)| epoch)
                {
                    continue;
                }
                let Some(data) = assembler.push(&packet) else {
                    continue;
                };
                if !thiscord_frontend::screen::bounded_parameter_sets(&data) {
                    break;
                }
                let Some(decoder) = decoder.as_mut() else {
                    continue;
                };
                let Ok(Some(yuv)) = decoder.decode(&data) else {
                    continue;
                };
                let (width, height) = yuv.dimensions();
                if width > MAX_WIDTH as usize || height > MAX_HEIGHT as usize {
                    break;
                }
                let mut rgb = vec![0; width * height * 3];
                yuv.write_rgb8(&mut rgb);
                let mut jpeg = Vec::new();
                if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
                    .encode(
                        &rgb,
                        width as u32,
                        height as u32,
                        image::ExtendedColorType::Rgb8,
                    )
                    .is_err()
                {
                    continue;
                }
                if let Ok(mut inner) = state.0.lock()
                    && connection.active()
                    && inner.owners[slot] == owner
                {
                    inner.frames[slot] = Some((Instant::now(), jpeg));
                }
            }
        });
        tx
    }
    pub fn frame(&self, slot: usize, owner: AccountId, epoch: u32) -> Option<Vec<u8>> {
        let inner = self.0.lock().ok()?;
        if !inner.binding.as_ref()?.connection.active() {
            return None;
        }
        if inner.owners.get(slot)? != &Some((owner, epoch)) {
            return None;
        }
        let (at, data) = inner.frames.get(slot)?.as_ref()?;
        (at.elapsed() < Duration::from_secs(2)).then(|| data.clone())
    }
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
    let mut sources = Vec::new();
    for monitor in xcap::Monitor::all().map_err(|_| "Cannot enumerate screens")? {
        if let (Ok(id), Ok(name)) = (monitor.id(), monitor.name()) {
            sources.push(Source {
                id: SourceId::Monitor(id),
                label: format!("Screen: {name}"),
            });
        }
    }
    for window in xcap::Window::all().map_err(|_| "Cannot enumerate windows")? {
        if let (Ok(id), Ok(title), Ok(false)) = (window.id(), window.title(), window.is_minimized())
            && !title.is_empty()
        {
            sources.push(Source {
                id: SourceId::Window(id),
                label: format!("Window: {title}"),
            });
        }
    }
    sources.truncate(256);
    Ok(sources)
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
    use openh264::{
        encoder::{BitRate, Encoder, EncoderConfig, FrameRate, Level, Profile, UsageType},
        formats::{RgbaSliceU8, YUVBuffer},
    };
    let monitor = match source {
        SourceId::Monitor(id) => Some(
            xcap::Monitor::all()
                .map_err(|_| "Cannot enumerate screens")?
                .into_iter()
                .find(|m| m.id().ok() == Some(*id))
                .ok_or("Selected screen is no longer available")?,
        ),
        _ => None,
    };
    let window = match source {
        SourceId::Window(id) => Some(
            xcap::Window::all()
                .map_err(|_| "Cannot enumerate windows")?
                .into_iter()
                .find(|w| w.id().ok() == Some(*id))
                .ok_or("Selected window is no longer available")?,
        ),
        _ => None,
    };
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(quality.bitrate()))
        .max_frame_rate(FrameRate::from_hz(quality.fps as f32))
        .profile(Profile::Baseline)
        .level(Level::Level_5_2)
        .usage_type(UsageType::ScreenContentRealTime);
    let mut encoder = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
        .map_err(|_| "Cannot initialize screen encoder")?;
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
            "{} ({}p, {} fps target)",
            if audio {
                "Sharing screen and system audio"
            } else {
                "Sharing screen"
            },
            quality.height,
            quality.fps
        );
    }
    let mut sequence = 0;
    let started = binding.started;
    let mut frame = 0;
    let mut previous = Instant::now();
    let result = (|| {
        while !stop.load(Ordering::Acquire) && binding.connection.active() {
            if previous.elapsed() > Duration::from_secs(2) {
                return Err("Screen capture paused; start sharing again.".into());
            }
            let tick = Instant::now();
            previous = tick;
            let captured = match (&monitor, &window) {
                (Some(m), _) => m.capture_image(),
                (_, Some(w)) => {
                    if w.is_minimized().unwrap_or(true) { return Err("The shared window was minimized or closed; sharing stopped.".into()); }
                    w.capture_image()
                },
                _ => unreachable!(),
            }.map_err(|_| "Screen capture failed. Check permissions and whether the source is still available.")?;
            let (w, h) = captured.dimensions();
            if w < 2 || h < 2 {
                return Err("Screen source has no visible content".into());
            }
            let scale = (quality.width() as f64 / w as f64)
                .min(quality.height as f64 / h as f64)
                .min(1.0);
            let w = ((w as f64 * scale) as u32 & !1).max(2);
            let h = ((h as f64 * scale) as u32 & !1).max(2);
            let rgba = if captured.dimensions() == (w, h) {
                captured
            } else {
                image::imageops::resize(&captured, w, h, image::imageops::FilterType::Triangle)
            };
            let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (w as usize, h as usize)));
            if frame % quality.fps == 0 {
                encoder.force_intra_frame();
            }
            let data = encoder
                .encode(&yuv)
                .map_err(|_| "Screen encoding failed")?
                .to_vec();
            if data.len() <= MAX_FRAME_BYTES {
                let packets = thiscord_frontend::screen::packetize(
                    data,
                    &mut sequence,
                    (started.elapsed().as_micros() * 90 / 1000) as u32,
                )?;
                if tx.try_send((tick, packets)).is_err() {
                    encoder.force_intra_frame();
                }
            }
            frame += 1;
            std::thread::sleep(
                Duration::from_secs_f64(1.0 / quality.fps as f64).saturating_sub(tick.elapsed()),
            );
        }
        Ok(())
    })();
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

pub fn protocol(
    app: &tauri::AppHandle,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
    let frame = if request.method() == tauri::http::Method::GET {
        frame_key(request.uri().path())
            .and_then(|(slot, epoch, owner)| app.state::<ScreenState>().frame(slot, owner, epoch))
    } else {
        None
    };
    tauri::http::Response::builder()
        .status(if frame.is_some() { 200 } else { 404 })
        .header("Content-Type", "image/jpeg")
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(frame.unwrap_or_default())
        .expect("screen response")
}

fn frame_key(path: &str) -> Option<(usize, u32, AccountId)> {
    let (slot, owner) = path.strip_prefix('/')?.split_once('-')?;
    let slot = slot.parse::<usize>().ok().filter(|s| *s < ROOM_CAPACITY)?;
    let (epoch, owner) = owner.split_once('-')?;
    Some((slot, epoch.parse().ok()?, owner.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding {
            connection: Connection::default(),
            video: thiscord_frontend::screen::track(901),
            audio: thiscord_frontend::audio::transport::track(902),
            can_publish: true,
            video_sequence: Default::default(),
            audio_sequence: Default::default(),
            audio_timestamp: Default::default(),
            started: Instant::now(),
        }
    }
    #[test]
    fn stop_and_new_voice_binding_cannot_revive_old_capture_or_cached_frames() {
        let state = ScreenState::default();
        state.bind(binding());
        let old_stop = Arc::new(AtomicBool::new(false));
        let owner: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let other: AccountId = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        {
            let mut inner = state.0.lock().unwrap();
            inner.stop = Some(old_stop.clone());
            inner.status.sharing = true;
            inner.owners[0] = Some((owner, 1));
            inner.frames[0] = Some((Instant::now(), vec![1, 2, 3]));
        }
        assert_eq!(frame_key(&format!("/0-1-{owner}")), Some((0, 1, owner)));
        assert_eq!(frame_key(&format!("/8-1-{owner}")), None);
        assert_eq!(state.frame(0, owner, 1), Some(vec![1, 2, 3]));
        assert!(state.frame(0, other, 1).is_none());
        assert!(state.frame(0, owner, 2).is_none());
        state.clear();
        assert!(old_stop.load(Ordering::Acquire));
        assert!(state.frame(0, owner, 1).is_none());
        assert_eq!(state.sharing(), (false, false));
        state.bind(binding());
        let new_stop = Arc::new(AtomicBool::new(false));
        {
            let mut inner = state.0.lock().unwrap();
            inner.stop = Some(new_stop.clone());
            inner.status.sharing = true;
        }
        state.finish(&old_stop, Some("late worker error".into()));
        assert!(state.sharing().0);
        assert!(!new_stop.load(Ordering::Acquire));
        state.roster(&[]);
        assert!(state.frame(0, owner, 1).is_none());
    }
}
