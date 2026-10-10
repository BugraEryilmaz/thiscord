//! Windows Graphics Capture delivers compositor frames; no screenshot polling.
//! The callback copies GPU textures only. A latest-frame mailbox decouples it
//! from the encoder, so overload replaces raw frames without breaking H.264 refs.
use super::{gpu, metrics::Metrics};
use crate::audio::connection::Connection;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{Quality, Source, SourceId};
use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*, Gdi::*};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

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
}
#[derive(Clone)]
struct Flags {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    stop: Arc<AtomicBool>,
    connection: Connection,
    period: Duration,
    quality: Quality,
    demand: Arc<super::adaptation::Demand>,
    metrics: Metrics,
}
struct Capture {
    flags: Flags,
    cadence: super::cadence::Cadence,
    textures: Vec<Arc<ID3D11Texture2D>>,
    dimensions: (u32, u32),
}
impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = String;
    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        Ok(Self {
            cadence: super::cadence::Cadence::new(ctx.flags.period),
            flags: ctx.flags,
            textures: Vec::new(),
            dimensions: (0, 0),
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), String> {
        if self.flags.stop.load(Ordering::Acquire) || !self.flags.connection.active() {
            control.stop();
            return Ok(());
        }
        let bitrate = self.flags.demand.bitrate();
        let fps = if bitrate == 0 {
            1
        } else {
            self.flags.quality.constrained(bitrate).fps
        };
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        if period != self.flags.period {
            self.flags.period = period;
            self.cadence = super::cadence::Cadence::new(period);
        }
        let now = Instant::now();
        self.flags.metrics.add("capture_events", 1);
        // Rate-limit work on compositor events, never sleep in the callback.
        if !self.cadence.accept(now) {
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
            self.textures = (0..3)
                .map(|_| {
                    gpu::texture(
                        frame.device(),
                        width,
                        height,
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                        D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    )
                    .map(Arc::new)
                })
                .collect::<windows::core::Result<_>>()
                .map_err(|e| format!("Capture texture allocation failed: {}", e.code()))?;
            self.dimensions = (width, height);
        }
        let Some(texture) = self.textures.iter().find(|t| Arc::strong_count(t) == 1) else {
            self.flags.metrics.add("gpu_busy", 1);
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
        let mut mailbox = self.flags.mailbox.0.lock().unwrap();
        if mailbox.latest.is_some() {
            self.flags.metrics.add("raw_replaced", 1);
        }
        mailbox.latest = Some(Pixels {
            texture: texture.clone(),
            device: frame.device().clone(),
            width,
            height,
            arrived: now,
        });
        self.flags.metrics.time("capture_submit", now.elapsed());
        self.flags.mailbox.1.notify_one();
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), String> {
        self.flags.mailbox.0.lock().unwrap().closed = true;
        self.flags.mailbox.1.notify_all();
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
fn minimized(window: Window) -> bool {
    // Handle was obtained from enumeration. Query only; never capture a fallback.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::IsIconic(windows::Win32::Foundation::HWND(
            window.as_raw_hwnd(),
        ))
        .as_bool()
    }
}
fn settings<T: TryInto<GraphicsCaptureItemType>>(item: T, flags: Flags) -> Settings<Flags, T> {
    Settings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        // IncludeSecondaryWindows defaults to false. Setting it explicitly
        // would unnecessarily require Windows 11 24H2; older WGC is sufficient.
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba16F,
        flags,
    )
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
    mut report: impl FnMut(String),
) -> Result<(), String> {
    let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
    let flags = Flags {
        mailbox: mailbox.clone(),
        stop: stop.clone(),
        connection: connection.clone(),
        period: Duration::from_secs_f64(1.0 / quality.fps as f64),
        quality,
        demand: demand.clone(),
        metrics: metrics.clone(),
    };
    let mut window = None;
    let capture = match source {
        SourceId::Monitor(id) => {
            let monitor = Monitor::enumerate()
                .map_err(|_| "Cannot enumerate screens")?
                .into_iter()
                .find(|m| m.as_raw_hmonitor() as u64 == *id)
                .ok_or("Selected screen is no longer available")?;
            Capture::start_free_threaded(settings(monitor, flags))
        }
        SourceId::Window(id) => {
            let selected = Window::enumerate()
                .map_err(|_| "Cannot enumerate windows")?
                .into_iter()
                .find(|w| w.as_raw_hwnd() as u64 == *id && w.is_valid() && !minimized(*w))
                .ok_or("Selected window is no longer available")?;
            window = Some(selected);
            Capture::start_free_threaded(settings(selected, flags))
        }
    }
    .map_err(|_| "Cannot start Windows Graphics Capture. Check capture permissions.")?;
    let result = encode(
        &mailbox,
        &capture,
        window,
        quality,
        &stop,
        &connection,
        started,
        tx,
        &mut report,
        source,
        &metrics,
        &force_keyframe,
        &demand,
    );
    // Stop also wakes/joins capture when the source is static (no callbacks).
    let stopped = capture
        .stop()
        .map_err(|_| "Cannot close screen capture".to_owned());
    result.and(stopped)
}

#[allow(clippy::too_many_arguments)]
fn encode(
    mailbox: &Arc<(Mutex<Mailbox>, Condvar)>,
    capture: &windows_capture::capture::CaptureControl<Capture, String>,
    window: Option<Window>,
    quality: Quality,
    stop: &AtomicBool,
    connection: &Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    report: &mut impl FnMut(String),
    source: &SourceId,
    metrics: &Metrics,
    force_keyframe: &AtomicBool,
    demand: &super::adaptation::Demand,
) -> Result<(), String> {
    let requested = quality;
    let mut quality = requested.constrained(demand.bitrate());
    let mut paused = true;
    let mut reconfigure = true;
    let mut last_raw: Option<Pixels> = None;
    let mut encode_cadence =
        super::cadence::Cadence::new(Duration::from_secs_f64(1.0 / f64::from(quality.fps)));
    let mut hardware: Option<super::hardware::HardwareEncoder> = None;
    let mut processor: Option<gpu::Processor> = None;
    let mut configured = None;
    let mut last_texture = None;
    let mut pending_pixels = None;
    let mut sequence = 0;
    let mut waiting_for_keyframe = true;
    let mut keyframe = Instant::now() - Duration::from_secs(1);
    let mut color_checked = Instant::now() - Duration::from_secs(2);
    let mut color = (1.0, false);
    let mut previous = Instant::now();
    let mut send_budget =
        super::pacing::SendBudget::new(super::pacing::transport_bitrate(quality.bitrate()));
    metrics.label("sender_backlog_budget_ms", super::pacing::BACKLOG_BUDGET_MS);
    metrics.label("video_paused", true);
    while !stop.load(Ordering::Acquire) && connection.active() {
        if previous.elapsed() > Duration::from_secs(2) {
            return Err("Screen capture paused; start sharing again.".into());
        }
        previous = Instant::now();
        if window.is_some_and(|w| !w.is_valid() || minimized(w)) || capture.is_finished() {
            return Err("The shared source closed or became unavailable; sharing stopped.".into());
        }
        let bitrate = demand.bitrate();
        if bitrate == 0 {
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
        let next = requested.constrained(bitrate);
        if paused || next != quality {
            quality = next;
            encode_cadence =
                super::cadence::Cadence::new(Duration::from_secs_f64(1.0 / f64::from(quality.fps)));
            hardware = None;
            processor = None;
            configured = None;
            last_texture = None;
            waiting_for_keyframe = true;
            reconfigure = true;
            paused = false;
            send_budget.set_bitrate(super::pacing::transport_bitrate(quality.bitrate()));
            force_keyframe.store(true, Ordering::Release);
            metrics.label("video_paused", false);
        }
        if color_checked.elapsed() >= Duration::from_secs(1) {
            let monitor = match source {
                SourceId::Monitor(id) => HMONITOR(*id as *mut _),
                SourceId::Window(id) => unsafe {
                    MonitorFromWindow(
                        windows::Win32::Foundation::HWND(*id as *mut _),
                        MONITOR_DEFAULTTONEAREST,
                    )
                },
            };
            color = gpu::display_white(monitor)
                .map_err(|e| format!("Cannot read display HDR calibration: {}", e.code()))?;
            metrics.label("hdr", color.1);
            metrics.label("sdr_white_nits", color.0 * 80.0);
            color_checked = Instant::now();
        }
        if let Some(hw) = hardware.as_mut() {
            metrics.peak("encoder_peak_pending", hw.pending() as u64);
            metrics.peak(
                "sender_peak_frames",
                (tx.max_capacity() - tx.capacity()) as u64,
            );
            let frames = hw
                .poll()
                .map_err(|e| format!("GPU encoder failed: {}", e.code()))?;
            record_frames(&frames, started, metrics);
            if !send_frames(
                frames,
                started,
                &tx,
                &mut sequence,
                &mut waiting_for_keyframe,
                metrics,
                &send_budget,
            )? {
                force_keyframe.store(true, Ordering::Release);
                metrics.event("encoded frame dropped before paced sender; requesting keyframe");
            }
        }
        let (pixels, fresh) = {
            let guard = mailbox.0.lock().unwrap();
            let (mut guard, _) = mailbox
                .1
                .wait_timeout_while(guard, Duration::from_millis(3), |m| {
                    m.latest.is_none() && !m.closed
                })
                .unwrap();
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
        if !encode_cadence.accept(Instant::now()) {
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
            metrics.label(
                "source_resolution",
                format!("{}x{}", pixels.width, pixels.height),
            );
            let scale = (quality.width() as f64 / pixels.width as f64)
                .min(quality.height as f64 / pixels.height as f64)
                .min(1.0);
            let w = ((pixels.width as f64 * scale) as u32 & !1).max(2);
            let h = ((pixels.height as f64 * scale) as u32 & !1).max(2);
            if configured != Some((w, h)) {
                hardware = Some(
                    super::hardware::HardwareEncoder::new_gpu(
                        w as usize,
                        h as usize,
                        quality,
                        &pixels.device,
                    )
                    .map_err(|e| format!("GPU H.264 surface encoding unavailable: {}", e.code()))?,
                );
                processor = Some(
                    gpu::Processor::new(&pixels.device, w, h)
                        .map_err(|e| format!("GPU video conversion unavailable: {}", e.code()))?,
                );
                metrics.label("resolution", format!("{w}x{h}"));
                metrics.label("target_fps", quality.fps);
                metrics.label("target_bitrate", quality.bitrate());
                metrics.label("encoder", &hardware.as_ref().unwrap().name);
                let hw = hardware.as_ref().unwrap();
                metrics.label(
                    "encoder_buffer_target_bytes",
                    super::hardware::buffer_bytes(quality.bitrate()),
                );
                metrics.label("encoder_buffer_applied", hw.buffer_applied);
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
                    "FP16 GPU / tone map / BT.709 NV12 / GPU encoder",
                );
                configured = Some((w, h));
                last_texture = None;
                force_keyframe.store(true, Ordering::Release);
                report(format!(
                    "GPU capture/scale/BT.709 NV12 -> {} (FP16 HDR-aware)",
                    hardware.as_ref().unwrap().name
                ));
            }
            if !hardware.as_ref().unwrap().ready() {
                metrics.add("gpu_busy", 1);
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
                continue;
            };
            last_texture = Some(texture);
        } else if keyframe.elapsed() < Duration::from_secs(1)
            && (!force_keyframe.load(Ordering::Acquire)
                || keyframe.elapsed() < Duration::from_millis(200))
        {
            continue;
        }
        let (Some(hw), Some(texture)) = (&mut hardware, &last_texture) else {
            continue;
        };
        if !hw.ready() {
            continue;
        }
        // Coalesce local backpressure and remote requests to avoid an IDR storm.
        let requested = keyframe.elapsed() >= Duration::from_millis(200)
            && force_keyframe.swap(false, Ordering::AcqRel);
        if requested || keyframe.elapsed() >= Duration::from_secs(1) {
            hw.force_keyframe()
                .map_err(|_| "GPU keyframe request failed")?;
            if requested {
                metrics.add("keyframe_feedback", 1);
            }
            keyframe = Instant::now();
        }
        let micros = captured_at.saturating_duration_since(started).as_micros() as u64;
        let frames = hw
            .encode_texture(texture.clone(), micros)
            .map_err(|e| format!("GPU encode failed: {}", e.code()))?;
        record_frames(&frames, started, metrics);
        if !send_frames(
            frames,
            started,
            &tx,
            &mut sequence,
            &mut waiting_for_keyframe,
            metrics,
            &send_budget,
        )? {
            force_keyframe.store(true, Ordering::Release);
        }
    }
    Ok(())
}
fn record_frames(frames: &[super::hardware::Encoded], started: Instant, metrics: &Metrics) {
    for frame in frames {
        metrics.add("encoded_frames", 1);
        metrics.add("encoded_bytes", frame.data.len() as u64);
        metrics.peak("encoded_peak_frame_bytes", frame.data.len() as u64);
        if super::recovery_frame(&frame.data) {
            metrics.add("keyframes", 1);
        }
        metrics.time(
            "encode_latency",
            (started + Duration::from_micros(frame.timestamp)).elapsed(),
        );
    }
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
