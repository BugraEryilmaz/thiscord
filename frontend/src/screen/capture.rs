//! Windows Graphics Capture delivers compositor frames; no screenshot polling.
//! The callback copies GPU textures only. A latest-frame mailbox decouples it
//! from the encoder, so overload replaces raw frames without breaking H.264 refs.
//! SDR displays are captured as 8-bit BGRA and converted by one video-processor
//! blit; HDR displays use FP16 scRGB and the tone-map shader. Turning HDR on
//! mid-share restarts the capture session in FP16 (one IDR), never clipping.
use super::{
    gpu::{self, SourceFormat},
    hardware::HardwareEncoder,
    metrics::Metrics,
    worker::{self, Inbox, Layer, LayerEncoder, Outcome, Pixels, Shared, Stops},
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
    frame::Frame as WgcFrame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

/// One owned capture texture on the capture device.
#[derive(Clone)]
pub struct Frame {
    texture: Arc<ID3D11Texture2D>,
    device: ID3D11Device,
}
#[derive(Clone)]
struct Flags {
    mailboxes: Vec<Inbox<Frame>>,
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
        frame: &mut WgcFrame,
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
        worker::deliver(
            &self.flags.mailboxes,
            Pixels {
                frame: Frame {
                    texture: texture.clone(),
                    device: frame.device().clone(),
                },
                width,
                height,
                arrived: captured_at,
            },
            &self.flags.metrics,
        );
        self.flags.metrics.time("capture_submit", now.elapsed());
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), String> {
        worker::close(&self.flags.mailboxes);
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
use worker::capture_fps;
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
                Outcome::Stopped => return Ok(()),
                Outcome::Restart => {
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
) -> Result<Outcome, String> {
    let mailbox = worker::inbox();
    let active_fps = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let low_mailbox = low.as_ref().map(|_| worker::inbox());
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
    let shared = Shared {
        stops: &stops,
        connection,
        started,
        active_fps: &active_fps,
        high_profile,
    };
    let platform = || Wgc {
        window,
        display,
        format,
        color: None,
        gpu_priority: false,
    };
    let finished = || capture.is_finished();
    let result = worker::run_layers(
        &mut platform(),
        &mailbox,
        Some(&finished),
        low.zip(low_mailbox)
            .map(|(low, mailbox)| (platform(), mailbox, low)),
        &shared,
        Layer {
            quality,
            tx: tx.clone(),
            metrics,
            force_keyframe,
            demand,
            secondary: false,
        },
        stop,
        report,
    );
    // Stop also wakes/joins capture when the source is static (no callbacks).
    let stopped = capture
        .stop()
        .map_err(|_| "Cannot close screen capture".to_owned());
    result.and_then(|outcome| stopped.map(|_| outcome))
}

/// Windows layer hooks: window validity, HDR state and MFT construction.
struct Wgc<'a> {
    window: Option<Window>,
    display: &'a Display,
    format: SourceFormat,
    color: Option<(f32, bool)>,
    gpu_priority: bool,
}
impl<'a> worker::Platform for Wgc<'a> {
    type Frame = Frame;
    type Encoder = Mft<'a>;
    fn source_lost(&mut self) -> bool {
        self.window.is_some_and(|w| !w.is_valid() || minimized(w))
    }
    fn tick(&mut self, metrics: &Metrics, secondary: bool) -> Result<bool, String> {
        let color = self.display.current()?;
        if self.color != Some(color) {
            metrics.label("hdr", color.1);
            metrics.label("sdr_white_nits", color.0 * 80.0);
            self.color = Some(color);
        }
        // 8-bit capture of an HDR composition would clip highlights. Only the
        // SDR->HDR direction restarts: FP16 remains correct (if costlier) on
        // SDR, so a window dragged across mixed displays cannot flap.
        Ok(!secondary && self.format == SourceFormat::Sdr && color.1)
    }
    fn encoder(
        &mut self,
        pixels: &Pixels<Frame>,
        (w, h): (u32, u32),
        quality: Quality,
        options: super::encode::EncoderOptions,
        wake: Box<dyn Fn() + Send + Sync>,
        metrics: &Metrics,
    ) -> Result<Mft<'a>, String> {
        let mut hw = HardwareEncoder::new_gpu_with(
            w as usize,
            h as usize,
            quality,
            &pixels.frame.device,
            options,
        )
        .map_err(|e| format!("GPU H.264 surface encoding unavailable: {}", e.code()))?;
        hw.notify(wake)
            .map_err(|e| format!("GPU encoder event subscription failed: {}", e.code()))?;
        if !self.gpu_priority {
            self.gpu_priority = true;
            metrics.label(
                "gpu_thread_priority",
                gpu::raise_gpu_priority(&pixels.frame.device),
            );
        }
        metrics.label(
            "encoder_buffer_target_bytes",
            super::hardware::buffer_bytes(quality.bitrate()),
        );
        metrics.label("encoder_buffer_applied", hw.buffer_applied);
        metrics.label("encoder_gop_applied", hw.gop_applied);
        metrics.label(
            "encoder_gop_readback_frames",
            hw.gop_readback_frames()
                .map_or_else(|| "unavailable".into(), |v| v.to_string()),
        );
        metrics.label(
            "encoder_buffer_readback_bytes",
            hw.buffer_readback_bytes()
                .map_or_else(|| "unavailable".into(), |v| v.to_string()),
        );
        if !hw.buffer_applied {
            metrics.event("encoder rejected low-latency buffer target; using byte admission");
        }
        metrics.label(
            "pixel_path",
            match self.format {
                SourceFormat::Hdr => "FP16 GPU / tone map / BT.709 NV12 / GPU encoder",
                SourceFormat::Sdr => {
                    "BGRA8 GPU / video-processor scale + BT.709 NV12 / GPU encoder"
                }
            },
        );
        Ok(Mft {
            hw,
            processor: None,
            display: self.display,
            format: self.format,
            size: (w, h),
        })
    }
}

/// Media Foundation hardware encoder plus its D3D11 NV12 processor.
struct Mft<'a> {
    hw: HardwareEncoder,
    processor: Option<gpu::Processor>,
    display: &'a Display,
    format: SourceFormat,
    size: (u32, u32),
}
impl LayerEncoder for Mft<'_> {
    type Frame = Frame;
    type Input = Arc<ID3D11Texture2D>;
    fn name(&self) -> &str {
        &self.hw.name
    }
    fn profile(&self) -> &'static str {
        self.hw.profile
    }
    fn pending(&self) -> usize {
        self.hw.pending()
    }
    fn ready(&self) -> bool {
        self.hw.ready()
    }
    fn poll(&mut self) -> Result<Vec<super::encode::Encoded>, String> {
        self.hw.poll().map_err(|e| e.code().to_string())
    }
    fn convert(&mut self, pixels: &Pixels<Frame>) -> Result<Option<Self::Input>, String> {
        // SDR scales from the capture surface, so a source resize needs a
        // new blit even when the encoded size is unchanged.
        if self.processor.as_ref().is_none_or(|p| {
            self.format == SourceFormat::Sdr && p.source() != (pixels.width, pixels.height)
        }) {
            let (w, h) = self.size;
            self.processor = Some(
                match self.format {
                    SourceFormat::Hdr => gpu::Processor::new(&pixels.frame.device, w, h),
                    SourceFormat::Sdr => gpu::Processor::new_sdr(
                        &pixels.frame.device,
                        (pixels.width, pixels.height),
                        w,
                        h,
                    ),
                }
                .map_err(|e| format!("GPU video conversion unavailable: {}", e.code()))?,
            );
        }
        let color = self.display.current()?;
        self.processor
            .as_ref()
            .expect("processor created above")
            .process(&pixels.frame.texture, color.0, color.1)
            .map_err(|e| format!("GPU tone mapping failed: {}", e.code()))
    }
    fn force_keyframe(&mut self) -> Result<(), String> {
        self.hw.force_keyframe().map_err(|e| e.code().to_string())
    }
    fn encode(
        &mut self,
        input: &Self::Input,
        micros: u64,
    ) -> Result<Vec<super::encode::Encoded>, String> {
        self.hw
            .encode_texture(input.clone(), micros)
            .map_err(|e| e.code().to_string())
    }
    fn quality_vs_speed(&self) -> Option<u32> {
        self.hw.quality_vs_speed()
    }
    fn set_quality_vs_speed(&mut self, value: u32) -> Result<(), String> {
        self.hw
            .set_quality_vs_speed(value)
            .map_err(|e| e.code().to_string())
    }
    fn describe(&self) -> String {
        format!(
            "GPU capture/scale/BT.709 NV12 -> {} ({}, {})",
            self.hw.name,
            match self.format {
                SourceFormat::Hdr => "FP16 HDR-aware",
                SourceFormat::Sdr => "SDR single blit",
            },
            self.hw.profile,
        )
    }
}
