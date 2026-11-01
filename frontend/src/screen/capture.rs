//! Windows Graphics Capture delivers compositor frames; no screenshot polling.
//! The callback copies GPU textures only. A latest-frame mailbox decouples it
//! from the encoder, so overload replaces raw frames without breaking H.264 refs.
//! SDR displays are captured as 8-bit BGRA and converted by one video-processor
//! blit; HDR displays use FP16 scRGB and the tone-map shader. Turning HDR on
//! mid-share restarts the capture session in FP16 (one IDR), never clipping.
use super::{
    gpu::{self, SourceFormat},
    hardware::{EncoderOptions, SpeedPolicy},
    keyframe::KeyframePolicy,
    metrics::Metrics,
};
use crate::audio::connection::Connection;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{Quality, Source, SourceId};
use windows::Win32::Graphics::{Direct3D11::*, Gdi::*};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

/// Window validity/minimized checks are syscalls; once per 100 ms is ample.
const WINDOW_CHECK: Duration = Duration::from_millis(100);
/// Longest encoder-worker sleep without a capture or MFT wake. It bounds how
/// long atomics (demand, feedback, stop) can wait to be observed.
const IDLE_WAIT: Duration = Duration::from_millis(20);
/// Short poll while a raw frame waits on time, sender backpressure or credit.
const PENDING_WAIT: Duration = Duration::from_millis(2);

#[derive(Clone)]
struct Pixels {
    texture: Arc<ID3D11Texture2D>,
    device: ID3D11Device,
    width: u32,
    height: u32,
    arrived: Instant,
}
#[derive(Default)]
struct Mailbox {
    latest: Option<Pixels>,
    closed: bool,
    encoder_event: bool,
}
#[derive(Clone)]
struct Flags {
    mailboxes: Vec<Arc<(Mutex<Mailbox>, Condvar)>>,
    stop: Arc<AtomicBool>,
    session: Arc<AtomicBool>,
    connection: Connection,
    period: Duration,
    active_fps: Arc<[AtomicU32; 2]>,
    metrics: Metrics,
    format: SourceFormat,
}
struct Capture {
    flags: Flags,
    cadence: super::cadence::Cadence,
    textures: Vec<Arc<ID3D11Texture2D>>,
    dimensions: (u32, u32),
    clock: (i64, Instant),
}
impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = String;
    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        use windows::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let (mut ticks, mut frequency) = (0, 0);
        unsafe {
            QueryPerformanceFrequency(&mut frequency).map_err(|_| "Cannot read capture clock")?;
            QueryPerformanceCounter(&mut ticks).map_err(|_| "Cannot read capture clock")?;
        }
        let clock = (
            (i128::from(ticks) * 10_000_000 / i128::from(frequency)) as i64,
            Instant::now(),
        );
        Ok(Self {
            cadence: super::cadence::Cadence::new(ctx.flags.period),
            flags: ctx.flags,
            textures: Vec::new(),
            dimensions: (0, 0),
            clock,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), String> {
        if self.flags.stop.load(Ordering::Acquire)
            || self.flags.session.load(Ordering::Acquire)
            || !self.flags.connection.active()
        {
            control.stop();
            return Ok(());
        }
        let fps = capture_fps(&self.flags.active_fps);
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        if period != self.flags.period {
            self.flags.period = period;
            self.cadence = super::cadence::Cadence::new(period);
        }
        let now = Instant::now();
        // RTP timing follows compositor render time, not callback scheduling.
        // Callback jitter otherwise turns evenly rendered frames into pairs that
        // the receiving compositor must discard on a 60 Hz presentation clock.
        let ticks = frame
            .timestamp()
            .map_err(|_| "Cannot timestamp capture")?
            .Duration;
        let delta = ticks.saturating_sub(self.clock.0);
        let offset = Duration::from_nanos(delta.unsigned_abs().saturating_mul(100));
        let captured_at = if delta >= 0 {
            self.clock.1.checked_add(offset)
        } else {
            self.clock.1.checked_sub(offset)
        }
        .ok_or("Invalid capture timestamp")?
        .min(now);
        self.flags.metrics.add("capture_events", 1);
        // Rate-limit work on compositor events, never sleep in the callback.
        if !self.cadence.accept(captured_at) {
            self.flags.metrics.add("capture_throttled", 1);
            return Ok(());
        }
        let (width, height) = (frame.width(), frame.height());
        if width < 2
            || height < 2
            || width > 16384
            || height > 16384
            || u64::from(width) * u64::from(height) > 33_554_432
        {
            return Err("Screen source dimensions are unsupported".into());
        }
        if self.dimensions != (width, height) {
            self.textures = (0..(if self.flags.mailboxes.len() == 1 {
                3
            } else {
                7
            }))
                .map(|_| {
                    gpu::texture(
                        frame.device(),
                        width,
                        height,
                        self.flags.format.dxgi(),
                        self.flags.format.bind_flags(),
                    )
                    .map(Arc::new)
                })
                .collect::<windows::core::Result<_>>()
                .map_err(|e| format!("Capture texture allocation failed: {}", e.code()))?;
            self.dimensions = (width, height);
        }
        let Some(texture) = self.textures.iter().find(|t| Arc::strong_count(t) == 1) else {
            self.flags.metrics.add("gpu_busy", 1);
            self.flags.metrics.add("capture_surface_busy", 1);
            return Ok(());
        };
        {
            let _guard = gpu::ContextGuard::lock(frame.device_context())
                .map_err(|_| "Cannot protect capture GPU context")?;
            unsafe {
                frame
                    .device_context()
                    .CopyResource(texture.as_ref(), frame.as_raw_texture());
                frame.device_context().Flush();
            }
        }
        for target in &self.flags.mailboxes {
            let mut mailbox = target.0.lock().unwrap();
            if mailbox.latest.is_some() {
                self.flags.metrics.add("raw_replaced", 1);
            }
            mailbox.latest = Some(Pixels {
                texture: texture.clone(),
                device: frame.device().clone(),
                width,
                height,
                arrived: captured_at,
            });
            target.1.notify_one();
        }
        self.flags.metrics.time("capture_submit", now.elapsed());
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), String> {
        for mailbox in &self.flags.mailboxes {
            mailbox.0.lock().unwrap().closed = true;
            mailbox.1.notify_all();
        }
        Ok(())
    }
}

pub fn sources() -> Result<Vec<Source>, String> {
    let mut sources = Vec::new();
    for monitor in Monitor::enumerate().map_err(|_| "Cannot enumerate screens")? {
        if let Ok(name) = monitor.name() {
            sources.push(Source {
                id: SourceId::Monitor(monitor.as_raw_hmonitor() as u64),
                label: format!("Screen: {name}"),
            });
        }
    }
    for window in Window::enumerate().map_err(|_| "Cannot enumerate windows")? {
        if window.is_valid()
            && !minimized(window)
            && let Ok(title) = window.title()
            && !title.is_empty()
        {
            sources.push(Source {
                id: SourceId::Window(window.as_raw_hwnd() as u64),
                label: title,
            });
        }
    }
    sources.truncate(256);
    Ok(sources)
}
// Each worker publishes only its own committed cadence. A paused or slower
// primary must not throttle a still-active low layer (and vice versa).
fn capture_fps(active_fps: &[AtomicU32; 2]) -> u32 {
    active_fps
        .iter()
        .map(|fps| fps.load(Ordering::Acquire))
        .max()
        .unwrap_or(0)
        .max(1)
}
fn minimized(window: Window) -> bool {
    // Handle was obtained from enumeration. Query only; never capture a fallback.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsIconic(windows::Win32::Foundation::HWND(
            window.as_raw_hwnd(),
        ))
        .as_bool()
    }
}
fn settings<T: TryInto<GraphicsCaptureItemType>>(
    item: T,
    flags: Flags,
    format: SourceFormat,
) -> Settings<Flags, T> {
    Settings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        // IncludeSecondaryWindows defaults to false. Setting it explicitly
        // would unnecessarily require Windows 11 24H2; older WGC is sufficient.
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        match format {
            SourceFormat::Hdr => ColorFormat::Rgba16F,
            SourceFormat::Sdr => ColorFormat::Bgra8,
        },
        flags,
    )
}

/// The selected display, or a window's dominant display (it can move).
fn monitor(source: &SourceId) -> HMONITOR {
    match source {
        SourceId::Monitor(id) => HMONITOR(*id as *mut _),
        SourceId::Window(id) => unsafe {
            MonitorFromWindow(
                windows::Win32::Foundation::HWND(*id as *mut _),
                MONITOR_DEFAULTTONEAREST,
            )
        },
    }
}
fn read_display(source: &SourceId) -> Result<(f32, bool), String> {
    gpu::display_white(monitor(source))
        .map_err(|e| format!("Cannot read display HDR calibration: {}", e.code()))
}

/// SDR white/HDR state refreshed once per second on its own thread.
/// `QueryDisplayConfig` can take milliseconds; encoders only read the cache.
struct Display {
    state: Mutex<Result<(f32, bool), String>>,
    stop: Mutex<bool>,
    wake: Condvar,
}
impl Display {
    fn current(&self) -> Result<(f32, bool), String> {
        self.state.lock().unwrap().clone()
    }
    fn refresh(&self, source: &SourceId) {
        loop {
            let stop = self.stop.lock().unwrap();
            let (stop, _) = self
                .wake
                .wait_timeout_while(stop, Duration::from_secs(1), |stop| !*stop)
                .unwrap();
            if *stop {
                return;
            }
            drop(stop);
            *self.state.lock().unwrap() = read_display(source);
        }
    }
    fn close(&self) {
        *self.stop.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Session {
    Stopped,
    /// An SDR session's display switched to HDR; recapture in FP16.
    HdrEnabled,
}
/// The user's stop ends sharing; a session stop only ends this capture format.
struct Stops<'a> {
    user: &'a AtomicBool,
    session: &'a AtomicBool,
}
impl Stops<'_> {
    fn stopped(&self) -> bool {
        self.user.load(Ordering::Acquire) || self.session.load(Ordering::Acquire)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    source: &SourceId,
    quality: Quality,
    stop: Arc<AtomicBool>,
    connection: Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    metrics: Metrics,
    force_keyframe: Arc<AtomicBool>,
    demand: Arc<super::adaptation::Demand>,
    report: impl FnMut(String),
) -> Result<(), String> {
    run_layers(
        source,
        quality,
        stop,
        connection,
        started,
        tx,
        metrics,
        force_keyframe,
        demand,
        None,
        Arc::default(),
        report,
    )
}

/// `high_profile` is one stream-wide flag for both layers (see `VideoProfile`).
#[allow(clippy::too_many_arguments)]
pub fn run_layers(
    source: &SourceId,
    quality: Quality,
    stop: Arc<AtomicBool>,
    connection: Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    metrics: Metrics,
    force_keyframe: Arc<AtomicBool>,
    demand: Arc<super::adaptation::Demand>,
    low: Option<super::LayerOutput>,
    high_profile: Arc<AtomicBool>,
    mut report: impl FnMut(String),
) -> Result<(), String> {
    let display = Display {
        state: Mutex::new(read_display(source)),
        stop: Mutex::new(false),
        wake: Condvar::new(),
    };
    std::thread::scope(|scope| {
        scope.spawn(|| display.refresh(source));
        let result = (|| loop {
            let format = if display.current()?.1 {
                SourceFormat::Hdr
            } else {
                SourceFormat::Sdr
            };
            metrics.label(
                "capture_format",
                match format {
                    SourceFormat::Hdr => "fp16_scrgb",
                    SourceFormat::Sdr => "bgra8",
                },
            );
            match session(
                source,
                quality,
                &stop,
                &connection,
                started,
                &tx,
                &metrics,
                &force_keyframe,
                &demand,
                low.as_ref(),
                &high_profile,
                &display,
                format,
                &mut report,
            )? {
                Session::Stopped => return Ok(()),
                Session::HdrEnabled => {
                    metrics.event("display switched to HDR; restarting capture in FP16");
                }
            }
        })();
        display.close();
        result
    })
}

#[allow(clippy::too_many_arguments)]
fn session(
    source: &SourceId,
    quality: Quality,
    stop: &Arc<AtomicBool>,
    connection: &Connection,
    started: Instant,
    tx: &tokio::sync::mpsc::Sender<super::Outgoing>,
    metrics: &Metrics,
    force_keyframe: &AtomicBool,
    demand: &super::adaptation::Demand,
    low: Option<&super::LayerOutput>,
    high_profile: &AtomicBool,
    display: &Display,
    format: SourceFormat,
    report: &mut impl FnMut(String),
) -> Result<Session, String> {
    let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
    let active_fps = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let low_mailbox = low
        .as_ref()
        .map(|_| Arc::new((Mutex::new(Mailbox::default()), Condvar::new())));
    let mut mailboxes = vec![mailbox.clone()];
    mailboxes.extend(low_mailbox.iter().cloned());
    let session_stop = Arc::new(AtomicBool::new(false));
    let flags = Flags {
        mailboxes,
        stop: stop.clone(),
        session: session_stop.clone(),
        connection: connection.clone(),
        period: Duration::from_secs_f64(1.0 / quality.fps as f64),
        active_fps: active_fps.clone(),
        metrics: metrics.clone(),
        format,
    };
    let mut window = None;
    let capture = match source {
        SourceId::Monitor(id) => {
            let monitor = Monitor::enumerate()
                .map_err(|_| "Cannot enumerate screens")?
                .into_iter()
                .find(|m| m.as_raw_hmonitor() as u64 == *id)
                .ok_or("Selected screen is no longer available")?;
            Capture::start_free_threaded(settings(monitor, flags, format))
        }
        SourceId::Window(id) => {
            let selected = Window::enumerate()
                .map_err(|_| "Cannot enumerate windows")?
                .into_iter()
                .find(|w| w.as_raw_hwnd() as u64 == *id && w.is_valid() && !minimized(*w))
                .ok_or("Selected window is no longer available")?;
            window = Some(selected);
            Capture::start_free_threaded(settings(selected, flags, format))
        }
    }
    .map_err(|_| "Cannot start Windows Graphics Capture. Check capture permissions.")?;
    let stops = Stops {
        user: stop,
        session: &session_stop,
    };
    let result = std::thread::scope(|scope| {
        let low_job = low.zip(low_mailbox).map(|(low, mailbox)| {
            let stops = &stops;
            let active_fps = &active_fps;
            let tx = low.tx.clone();
            scope.spawn(move || {
                // A panicking low encoder must still end the share and its session.
                struct StopOnExit<'a>(&'a Stops<'a>);
                impl Drop for StopOnExit<'_> {
                    fn drop(&mut self) {
                        self.0.session.store(true, Ordering::Release);
                        if std::thread::panicking() {
                            self.0.user.store(true, Ordering::Release);
                        }
                    }
                }
                let _stop_on_exit = StopOnExit(stops);
                let result = encode(
                    &mailbox,
                    None,
                    window,
                    thiscord_shared::screen::LayerTargets::low_quality(quality),
                    stops,
                    connection,
                    started,
                    tx,
                    &mut |_| {},
                    &low.metrics,
                    &low.force_keyframe,
                    &low.demand,
                    active_fps,
                    true,
                    format,
                    display,
                    high_profile,
                );
                if result.is_err() {
                    stops.user.store(true, Ordering::Release);
                }
                result
            })
        });
        let result = encode(
            &mailbox,
            Some(&capture),
            window,
            quality,
            &stops,
            connection,
            started,
            tx.clone(),
            report,
            metrics,
            force_keyframe,
            demand,
            &active_fps,
            false,
            format,
            display,
            high_profile,
        );
        // A format restart only ends this session; anything else ends sharing.
        if result != Ok(Session::HdrEnabled) {
            stop.store(true, Ordering::Release);
        }
        session_stop.store(true, Ordering::Release);
        let secondary = low_job.map_or(Ok(Session::Stopped), |job| {
            job.join()
                .unwrap_or_else(|_| Err("Screen low encoder worker failed".into()))
        });
        result.and_then(|session| secondary.map(|_| session))
    });
    // Stop also wakes/joins capture when the source is static (no callbacks).
    let stopped = capture
        .stop()
        .map_err(|_| "Cannot close screen capture".to_owned());
    result.and_then(|session| stopped.map(|_| session))
}

#[allow(clippy::too_many_arguments)]
fn encode(
    mailbox: &Arc<(Mutex<Mailbox>, Condvar)>,
    capture: Option<&windows_capture::capture::CaptureControl<Capture, String>>,
    window: Option<Window>,
    quality: Quality,
    stops: &Stops,
    connection: &Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    report: &mut impl FnMut(String),
    metrics: &Metrics,
    force_keyframe: &AtomicBool,
    demand: &super::adaptation::Demand,
    active_fps: &[AtomicU32; 2],
    secondary: bool,
    format: SourceFormat,
    display: &Display,
    high_profile: &AtomicBool,
) -> Result<Session, String> {
    metrics.label(
        "encoder_thread_priority",
        super::priority::promote_video_thread(),
    );
    let requested = quality;
    let mut adaptation = super::adaptation::QualityController::new(requested);
    let mut quality = requested.constrained(demand.bitrate());
    let mut high = high_profile.load(Ordering::Acquire);
    let mut paused = true;
    let mut reconfigure = true;
    let mut last_raw: Option<Pixels> = None;
    let mut refresh_after = Instant::now();
    let mut input_after = Instant::now();
    let mut hardware: Option<super::hardware::HardwareEncoder> = None;
    let mut processor: Option<gpu::Processor> = None;
    let mut configured = None;
    let mut last_texture = None;
    let mut pending_pixels = None;
    let mut sequence = 0;
    let mut waiting_for_keyframe = true;
    let mut keyframe = KeyframePolicy::default();
    let mut speed = SpeedPolicy::default();
    let mut gpu_priority = false;
    let mut color;
    let mut previous_color = None;
    let mut previous = Instant::now();
    let mut previous_bitrate = None;
    let mut window_checked = Instant::now() - WINDOW_CHECK;
    let mut last_source = None;
    let mut recovery_scale = 100;
    let mut send_budget =
        super::pacing::SendBudget::new(super::pacing::transport_bitrate(quality.bitrate()));
    metrics.label("sender_backlog_budget_ms", super::pacing::BACKLOG_BUDGET_MS);
    metrics.label("requested_fps", requested.fps);
    metrics.label("requested_height", requested.height);
    metrics.label("video_paused", true);
    while !stops.stopped() && connection.active() {
        if previous.elapsed() > Duration::from_secs(2) {
            return Err("Screen capture paused; start sharing again.".into());
        }
        previous = Instant::now();
        if window_checked.elapsed() >= WINDOW_CHECK {
            window_checked = Instant::now();
            if window.is_some_and(|w| !w.is_valid() || minimized(w)) {
                return Err(
                    "The shared source closed or became unavailable; sharing stopped.".into(),
                );
            }
        }
        if mailbox.0.lock().unwrap().closed || capture.is_some_and(|c| c.is_finished()) {
            return Err("The shared source closed or became unavailable; sharing stopped.".into());
        }
        color = display.current()?;
        if previous_color != Some(color) {
            metrics.label("hdr", color.1);
            metrics.label("sdr_white_nits", color.0 * 80.0);
            previous_color = Some(color);
        }
        // 8-bit capture of an HDR composition would clip highlights. Only the
        // SDR->HDR direction restarts: FP16 remains correct (if costlier) on
        // SDR, so a window dragged across mixed displays cannot flap.
        if !secondary && format == SourceFormat::Sdr && color.1 {
            return Ok(Session::HdrEnabled);
        }
        let bitrate = demand.bitrate();
        if previous_bitrate != Some(bitrate) {
            metrics.label("viewer_bitrate_budget", bitrate);
            previous_bitrate = Some(bitrate);
        }
        let next = adaptation.update_with_recovery(
            bitrate,
            demand.take_encoder_feedback(),
            Instant::now(),
        );
        if next.is_none() {
            active_fps[usize::from(secondary)].store(0, Ordering::Release);
            if !paused {
                hardware = None;
                processor = None;
                configured = None;
                last_texture = None;
                waiting_for_keyframe = true;
                metrics.label("video_paused", true);
            }
            paused = true;
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let next = next.expect("active demand has a quality target");
        // A profile change rebuilds like a quality change: the new bitstream
        // starts with SPS/PPS/IDR, so viewers switch decoders on a keyframe.
        let want_high = high_profile.load(Ordering::Acquire);
        if paused
            || next != quality
            || recovery_scale != adaptation.scale_percent()
            || want_high != high
        {
            quality = next;
            high = want_high;
            recovery_scale = adaptation.scale_percent();
            active_fps[usize::from(secondary)].store(quality.fps, Ordering::Release);
            refresh_after = Instant::now();
            hardware = None;
            processor = None;
            configured = None;
            last_texture = None;
            waiting_for_keyframe = true;
            reconfigure = true;
            paused = false;
            force_keyframe.store(true, Ordering::Release);
            metrics.label("video_paused", false);
            metrics.label("recovery_scale_percent", recovery_scale);
            metrics.label(
                "encoder_profile_requested",
                if high { "high" } else { "baseline" },
            );
        }
        // Share live demand with the sender, retaining burst capacity even after
        // a recovery downgrade. Shed raw input at this rate before encoding more
        // dependencies; a small budget dip must not halve the transport rate.
        send_budget.set_bitrate(super::pacing::transport_budget(quality.bitrate(), bitrate));
        let (pixels, fresh) = {
            let guard = mailbox.0.lock().unwrap();
            let now = Instant::now();
            // Sleep until real work can be due: a capture/MFT notification wakes
            // the condvar early. Pending input polls briefly for time, credit or
            // sender drain; otherwise wait for the next static refresh.
            let timeout = if pending_pixels.is_some() {
                PENDING_WAIT
            } else if hardware.is_some() && last_texture.is_some() {
                keyframe
                    .next_refresh()
                    .map_or(Duration::ZERO, |at| {
                        at.max(refresh_after).saturating_duration_since(now)
                    })
                    .min(IDLE_WAIT)
            } else {
                IDLE_WAIT
            }
            .max(Duration::from_millis(1));
            let (mut guard, _) = mailbox
                .1
                .wait_timeout_while(guard, timeout, |m| {
                    m.latest.is_none() && !m.closed && !m.encoder_event
                })
                .unwrap();
            guard.encoder_event = false;
            if guard.closed {
                return Err("The shared source closed; sharing stopped.".into());
            }
            // Consume the old pending texture even when a newer capture wins;
            // retaining it would submit an older timestamp after the new frame.
            let pending = pending_pixels.take();
            if guard.latest.is_some() && pending.is_some() {
                metrics.add("raw_replaced", 1);
            }
            let fresh = guard.latest.is_some();
            (guard.latest.take().or(pending), fresh)
        };
        // Capture waiting can outlast an MFT NeedInput/HaveOutput event. Drain
        // after waking, before testing encoder readiness for this new frame.
        let mut rebuild = false;
        if let Some(hw) = hardware.as_mut() {
            metrics.peak("encoder_peak_pending", hw.pending() as u64);
            metrics.peak(
                "sender_peak_frames",
                (tx.max_capacity() - tx.capacity()) as u64,
            );
            let frames = hw
                .poll()
                .map_err(|e| format!("GPU encoder failed: {}", e.code()))?;
            if record_frames(&frames, started, metrics) {
                keyframe.observed(Instant::now());
            }
            rebuild = observe_speed(&frames, &mut speed, hw, quality.fps, metrics);
            if !send_frames(
                frames,
                started,
                &tx,
                &mut sequence,
                &mut waiting_for_keyframe,
                metrics,
                &send_budget,
                quality.bitrate(),
            )? {
                force_keyframe.store(true, Ordering::Release);
                metrics.event("encoded frame dropped before paced sender; requesting keyframe");
            }
        }
        if rebuild {
            hardware = None;
            processor = None;
            configured = None;
            last_texture = None;
            waiting_for_keyframe = true;
            reconfigure = true;
            force_keyframe.store(true, Ordering::Release);
        }
        let pixels = pixels
            .map(|mut p| {
                if reconfigure {
                    p.arrived = Instant::now();
                }
                p
            })
            .or_else(|| {
                if reconfigure {
                    last_raw.clone().map(|mut p| {
                        p.arrived = Instant::now();
                        p
                    })
                } else {
                    None
                }
            });
        // Fresh input was already rate-limited on the compositor clock. Gating
        // it again on worker wake time loses frames when a previous submission
        // was delayed, even though the encoder has ample throughput. The second
        // cadence only limits refresh/feedback submissions of the held image.
        if (pixels.is_none() && Instant::now() < refresh_after)
            || (capture_fps(active_fps) > quality.fps && Instant::now() < input_after)
        {
            pending_pixels = pixels;
            continue;
        }
        let mut captured_at = Instant::now();
        let queued = tx.max_capacity() - tx.capacity();
        if !super::encoder_has_capacity(
            super::sender_queue_frames(quality.fps).saturating_sub(queued),
            hardware.as_ref().map_or(0, |hw| hw.pending()),
        ) || !send_budget.has_capacity()
        {
            // Dropping raw input preserves the H.264 dependency chain. Poll
            // hardware above even when congested, but submit no new surfaces.
            if fresh {
                metrics.add("encode_backpressure", 1);
            }
            // Retain the latest raw texture so a source that becomes static
            // during backpressure still delivers its final update. New capture
            // replaces this texture before it becomes an H.264 dependency.
            pending_pixels = pixels;
            continue;
        }
        if let Some(pixels) = pixels {
            if pixels.arrived.elapsed() > Duration::from_millis(250) {
                metrics.add("expired_frames", 1);
                continue;
            }
            captured_at = pixels.arrived;
            last_raw = Some(pixels.clone());
            reconfigure = false;
            if last_source != Some((pixels.width, pixels.height)) {
                last_source = Some((pixels.width, pixels.height));
                metrics.label(
                    "source_resolution",
                    format!("{}x{}", pixels.width, pixels.height),
                );
            }
            let scale =
                (quality.width() as f64 * f64::from(recovery_scale) / 100.0 / pixels.width as f64)
                    .min(
                        quality.height as f64 * f64::from(recovery_scale)
                            / 100.0
                            / pixels.height as f64,
                    )
                    .min(1.0);
            let w = ((pixels.width as f64 * scale) as u32 & !1).max(2);
            let h = ((pixels.height as f64 * scale) as u32 & !1).max(2);
            if configured != Some((w, h)) {
                let options = EncoderOptions {
                    high_profile: high,
                    quality_speed: speed.hint(w as usize, h as usize, quality.fps),
                };
                speed.reset_window();
                hardware = Some(
                    super::hardware::HardwareEncoder::new_gpu_with(
                        w as usize,
                        h as usize,
                        quality,
                        &pixels.device,
                        options,
                    )
                    .map_err(|e| format!("GPU H.264 surface encoding unavailable: {}", e.code()))?,
                );
                let wake = mailbox.clone();
                hardware
                    .as_mut()
                    .unwrap()
                    .notify(move || {
                        wake.0.lock().unwrap().encoder_event = true;
                        wake.1.notify_one();
                    })
                    .map_err(|e| format!("GPU encoder event subscription failed: {}", e.code()))?;
                processor = None;
                if !gpu_priority {
                    gpu_priority = true;
                    metrics.label(
                        "gpu_thread_priority",
                        gpu::raise_gpu_priority(&pixels.device),
                    );
                }
                metrics.label("resolution", format!("{w}x{h}"));
                metrics.label("target_fps", quality.fps);
                metrics.label("target_bitrate", quality.bitrate());
                metrics.label("encoder", &hardware.as_ref().unwrap().name);
                let hw = hardware.as_ref().unwrap();
                metrics.label("encoder_profile", hw.profile);
                metrics.label(
                    "encoder_buffer_target_bytes",
                    super::hardware::buffer_bytes(quality.bitrate()),
                );
                metrics.label("encoder_buffer_applied", hw.buffer_applied);
                metrics.label(
                    "encoder_gop_target_frames",
                    super::keyframe::gop_frames(quality.fps),
                );
                metrics.label("encoder_gop_applied", hw.gop_applied);
                metrics.label(
                    "encoder_gop_readback_frames",
                    hw.gop_readback_frames()
                        .map_or_else(|| "unavailable".into(), |v| v.to_string()),
                );
                metrics.label(
                    "encoder_quality_vs_speed",
                    hw.quality_vs_speed()
                        .map_or_else(|| "unavailable".into(), |v| v.to_string()),
                );
                metrics.label(
                    "encoder_buffer_readback_bytes",
                    hw.buffer_readback_bytes()
                        .map_or_else(|| "unavailable".into(), |v| v.to_string()),
                );
                if !hw.buffer_applied {
                    metrics
                        .event("encoder rejected low-latency buffer target; using byte admission");
                }
                metrics.label(
                    "pixel_path",
                    match format {
                        SourceFormat::Hdr => "FP16 GPU / tone map / BT.709 NV12 / GPU encoder",
                        SourceFormat::Sdr => {
                            "BGRA8 GPU / video-processor scale + BT.709 NV12 / GPU encoder"
                        }
                    },
                );
                configured = Some((w, h));
                last_texture = None;
                keyframe = KeyframePolicy::default();
                force_keyframe.store(true, Ordering::Release);
                report(format!(
                    "GPU capture/scale/BT.709 NV12 -> {} ({}, {})",
                    hardware.as_ref().unwrap().name,
                    match format {
                        SourceFormat::Hdr => "FP16 HDR-aware",
                        SourceFormat::Sdr => "SDR single blit",
                    },
                    hardware.as_ref().unwrap().profile,
                ));
            }
            // SDR scales from the capture surface, so a source resize needs a
            // new blit even when the encoded size is unchanged.
            if processor.as_ref().is_none_or(|p| {
                format == SourceFormat::Sdr && p.source() != (pixels.width, pixels.height)
            }) {
                processor = Some(
                    match format {
                        SourceFormat::Hdr => gpu::Processor::new(&pixels.device, w, h),
                        SourceFormat::Sdr => gpu::Processor::new_sdr(
                            &pixels.device,
                            (pixels.width, pixels.height),
                            w,
                            h,
                        ),
                    }
                    .map_err(|e| format!("GPU video conversion unavailable: {}", e.code()))?,
                );
            }
            if !hardware.as_ref().unwrap().ready() {
                metrics.add("gpu_busy", 1);
                metrics.add("encoder_input_busy", 1);
                pending_pixels = Some(pixels);
                continue;
            }
            let at = Instant::now();
            let texture = processor
                .as_ref()
                .unwrap()
                .process(&pixels.texture, color.0, color.1)
                .map_err(|e| format!("GPU tone mapping failed: {}", e.code()))?;
            metrics.time("gpu_submit", at.elapsed());
            let Some(texture) = texture else {
                metrics.add("gpu_busy", 1);
                metrics.add("conversion_surface_busy", 1);
                continue;
            };
            last_texture = Some(texture);
        } else if !keyframe.refresh_due(Instant::now(), force_keyframe.load(Ordering::Acquire)) {
            continue;
        }
        let (Some(hw), Some(texture)) = (&mut hardware, &last_texture) else {
            continue;
        };
        if !hw.ready() {
            continue;
        }
        // Coalesce local backpressure and remote requests to avoid an IDR storm.
        let now = Instant::now();
        let requested =
            keyframe.feedback_ready(now) && force_keyframe.swap(false, Ordering::AcqRel);
        let forced = requested || keyframe.periodic_due(now);
        if forced {
            hw.force_keyframe()
                .map_err(|_| "GPU keyframe request failed")?;
            if requested {
                metrics.add("keyframe_feedback", 1);
            }
        }
        let micros = captured_at.saturating_duration_since(started).as_micros() as u64;
        // Empty polls, busy hardware and raw-input backpressure must not advance
        // the encoder clock ahead of the separately throttled capture callback.
        refresh_after = Instant::now() + Duration::from_secs_f64(1.0 / f64::from(quality.fps));
        input_after = refresh_after;
        metrics.time("encode_input_age", captured_at.elapsed());
        let frames = hw
            .encode_texture(texture.clone(), micros)
            .map_err(|e| format!("GPU encode failed: {}", e.code()))?;
        keyframe.submitted(now, forced);
        if record_frames(&frames, started, metrics) {
            keyframe.observed(Instant::now());
        }
        let rebuild = observe_speed(&frames, &mut speed, hw, quality.fps, metrics);
        if !send_frames(
            frames,
            started,
            &tx,
            &mut sequence,
            &mut waiting_for_keyframe,
            metrics,
            &send_budget,
            quality.bitrate(),
        )? {
            force_keyframe.store(true, Ordering::Release);
        }
        if rebuild {
            hardware = None;
            processor = None;
            configured = None;
            last_texture = None;
            waiting_for_keyframe = true;
            reconfigure = true;
            force_keyframe.store(true, Ordering::Release);
        }
    }
    Ok(Session::Stopped)
}
/// Feed MFT service times to the speed policy. Returns true when the encoder
/// must be rebuilt because the driver ignored a live speed-hint change.
fn observe_speed(
    frames: &[super::hardware::Encoded],
    speed: &mut SpeedPolicy,
    hw: &super::hardware::HardwareEncoder,
    fps: u32,
    metrics: &Metrics,
) -> bool {
    if hw.quality_vs_speed() == Some(0) {
        return false;
    }
    for frame in frames {
        if speed.observe(frame.encoding_duration, fps) {
            metrics.event("encoder service near frame interval; using fastest speed hint");
            if hw.set_quality_vs_speed(0).is_ok() {
                metrics.label("encoder_quality_vs_speed", 0);
                return false;
            }
            return true;
        }
    }
    false
}
fn record_frames(frames: &[super::hardware::Encoded], started: Instant, metrics: &Metrics) -> bool {
    let mut keyframe = false;
    for frame in frames {
        metrics.add("encoded_frames", 1);
        metrics.add("encoded_bytes", frame.data.len() as u64);
        metrics.peak("encoded_peak_frame_bytes", frame.data.len() as u64);
        if super::recovery_frame(&frame.data) {
            metrics.add("keyframes", 1);
            keyframe = true;
        }
        metrics.time(
            "encode_latency",
            (started + Duration::from_micros(frame.timestamp)).elapsed(),
        );
        metrics.time("encoder_service", frame.encoding_duration);
    }
    keyframe
}

#[allow(clippy::too_many_arguments)]
fn send_frames(
    frames: Vec<super::hardware::Encoded>,
    started: Instant,
    tx: &tokio::sync::mpsc::Sender<super::Outgoing>,
    sequence: &mut u16,
    waiting_for_keyframe: &mut bool,
    metrics: &Metrics,
    send_budget: &super::pacing::SendBudget,
    bitrate: u32,
) -> Result<bool, String> {
    let mut complete = true;
    for frame in frames {
        if frame.data.is_empty() {
            continue;
        }
        let at = started
            .checked_add(Duration::from_micros(frame.timestamp))
            .ok_or("Invalid video timestamp")?;
        if at.elapsed() > super::SENDER_QUEUE_AGE {
            *waiting_for_keyframe = true;
            metrics.add("encode_dropped", 1);
            metrics.add("encode_stale", 1);
            complete = false;
            continue;
        }
        if *waiting_for_keyframe && !super::recovery_frame(&frame.data) {
            metrics.add("encode_dropped", 1);
            metrics.add("encode_dependent", 1);
            // Recovery is already requested. Do not relatch it for each delta.
            continue;
        }
        let recovery = super::recovery_frame(&frame.data);
        let packets = super::packetize(frame.data, sequence, (frame.timestamp * 90 / 1000) as u32)?;
        let budget = send_budget.track(packets.iter().map(|p| p.payload.len() + 64).sum());
        metrics.peak("sender_peak_bytes", send_budget.bytes() as u64);
        if tx
            .try_send(super::Outgoing {
                bitrate,
                captured_at: at,
                enqueued_at: Instant::now(),
                packets,
                keyframe: recovery,
                budget,
            })
            .is_err()
        {
            *waiting_for_keyframe = true;
            metrics.add("encode_dropped", 1);
            metrics.add("encode_queue_full", 1);
            complete = false;
        } else {
            *waiting_for_keyframe = false;
        }
    }
    Ok(complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_cadence_follows_both_committed_layers_through_pause_and_recovery() {
        let fps = [AtomicU32::new(60), AtomicU32::new(15)];
        assert_eq!(capture_fps(&fps), 60);
        fps[0].store(5, Ordering::Release);
        assert_eq!(capture_fps(&fps), 15);
        fps[0].store(0, Ordering::Release);
        assert_eq!(capture_fps(&fps), 15);
        fps[1].store(0, Ordering::Release);
        assert_eq!(capture_fps(&fps), 1);
        fps[0].store(30, Ordering::Release);
        assert_eq!(capture_fps(&fps), 30);
    }

    #[test]
    fn encoded_backpressure_discards_pending_deltas_until_a_recovery_frame() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let started = Instant::now();
        let mut sequence = 0;
        let mut waiting = true;
        let metrics = Metrics::default();
        let send_budget = super::super::pacing::SendBudget::new(21_600_000);
        let frame = |key| super::super::hardware::Encoded {
            data: if key {
                vec![0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
            } else {
                vec![0, 0, 1, 0x41, 1]
            },
            timestamp: 0,
            encoding_duration: Duration::ZERO,
        };
        assert!(
            send_frames(
                vec![frame(true)],
                started,
                &tx,
                &mut sequence,
                &mut waiting,
                &metrics,
                &send_budget,
                5_000_000,
            )
            .unwrap()
        );
        assert!(
            !send_frames(
                vec![frame(false)],
                started,
                &tx,
                &mut sequence,
                &mut waiting,
                &metrics,
                &send_budget,
                5_000_000,
            )
            .unwrap()
        );
        rx.try_recv().unwrap();
        assert!(
            send_frames(
                vec![frame(false)],
                started,
                &tx,
                &mut sequence,
                &mut waiting,
                &metrics,
                &send_budget,
                5_000_000,
            )
            .unwrap()
        );
        assert!(
            rx.try_recv().is_err(),
            "dependent hardware output must not follow a dropped frame"
        );
        assert!(
            send_frames(
                vec![frame(true)],
                started,
                &tx,
                &mut sequence,
                &mut waiting,
                &metrics,
                &send_budget,
                5_000_000,
            )
            .unwrap()
        );
        assert!(!waiting);
        assert_eq!(metrics.snapshot().counters["encode_dropped"], 2);
        assert_eq!(metrics.snapshot().counters["encode_queue_full"], 1);
        assert_eq!(metrics.snapshot().counters["encode_dependent"], 1);
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            send_budget.bytes(),
            0,
            "failed enqueue and frame drops release bytes"
        );
    }
}
