//! Windows Graphics Capture delivers compositor frames; no screenshot polling.
//! The callback owns capture/readback only. A latest-frame mailbox decouples it
//! from the encoder, so overload replaces raw frames without breaking H.264 refs.
use crate::audio::connection::Connection;
use openh264::{
    encoder::{BitRate, Encoder, EncoderConfig, FrameRate, Level, Profile, UsageType},
    formats::{RgbaSliceU8, YUVBuffer, YUVSource},
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{MAX_FRAME_BYTES, Quality, Source, SourceId};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::*,
    window::Window,
};

struct Pixels {
    rgba: image::RgbaImage,
    arrived: Instant,
}
#[derive(Default)]
struct Mailbox {
    latest: Option<Pixels>,
    spare: Vec<u8>,
    closed: bool,
}
#[derive(Clone)]
struct Flags {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    stop: Arc<AtomicBool>,
    connection: Connection,
    period: Duration,
}
struct Capture {
    flags: Flags,
    cadence: super::cadence::Cadence,
}
impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = String;
    fn new(ctx: Context<Flags>) -> Result<Self, String> {
        Ok(Self {
            cadence: super::cadence::Cadence::new(ctx.flags.period),
            flags: ctx.flags,
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
        let now = Instant::now();
        // Rate-limit work on compositor events, never sleep in the callback.
        if !self.cadence.accept(now) {
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
        let mut bytes = {
            let mut mailbox = self.flags.mailbox.0.lock().unwrap();
            // Reuse the dropped frame's storage before allocating another.
            mailbox
                .latest
                .take()
                .map(|p| p.rgba.into_raw())
                .unwrap_or_else(|| std::mem::take(&mut mailbox.spare))
        };
        let mut buffer = frame
            .buffer()
            .map_err(|_| "Cannot read screen capture frame")?;
        let stride = buffer.row_pitch() as usize;
        let row_bytes = width as usize * 4;
        bytes.resize(row_bytes * height as usize, 0);
        for (source, target) in buffer
            .as_raw_buffer()
            .chunks(stride)
            .zip(bytes.chunks_mut(row_bytes))
        {
            target.copy_from_slice(&source[..row_bytes]);
        }
        let rgba = image::RgbaImage::from_raw(width, height, bytes)
            .ok_or("Invalid screen capture frame")?;
        let mut mailbox = self.flags.mailbox.0.lock().unwrap();
        mailbox.latest = Some(Pixels { rgba, arrived: now });
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
        ColorFormat::Rgba8,
        flags,
    )
}

pub fn run(
    source: &SourceId,
    quality: Quality,
    stop: Arc<AtomicBool>,
    connection: Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<(Instant, Vec<rtc::rtp::Packet>)>,
    mut report: impl FnMut(String),
) -> Result<(), String> {
    let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
    let flags = Flags {
        mailbox: mailbox.clone(),
        stop: stop.clone(),
        connection: connection.clone(),
        period: Duration::from_secs_f64(1.0 / quality.fps as f64),
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
    tx: tokio::sync::mpsc::Sender<(Instant, Vec<rtc::rtp::Packet>)>,
    report: &mut impl FnMut(String),
) -> Result<(), String> {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(quality.bitrate()))
        .max_frame_rate(FrameRate::from_hz(quality.fps as f32))
        .profile(Profile::Baseline)
        .level(Level::Level_5_2)
        .usage_type(UsageType::ScreenContentRealTime);
    let mut encoder = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
        .map_err(|_| "Cannot initialize screen encoder")?;
    let mut yuv: Option<YUVBuffer> = None;
    let mut hardware: Option<super::hardware::HardwareEncoder> = None;
    let mut configured = None;
    let mut sequence = 0;
    let mut waiting_for_keyframe = true;
    let mut keyframe = Instant::now() - Duration::from_secs(1);
    let mut previous = Instant::now();
    while !stop.load(Ordering::Acquire) && connection.active() {
        if previous.elapsed() > Duration::from_secs(2) {
            return Err("Screen capture paused; start sharing again.".into());
        }
        previous = Instant::now();
        if window.is_some_and(|w| !w.is_valid() || minimized(w)) || capture.is_finished() {
            return Err("The shared source closed or became unavailable; sharing stopped.".into());
        }
        if let Some(hw) = hardware.as_mut() {
            match hw.poll() {
                Ok(frames) => {
                    if !send_frames(
                        frames,
                        started,
                        &tx,
                        &mut sequence,
                        &mut waiting_for_keyframe,
                    )? {
                        let _ = hw.force_keyframe();
                    }
                }
                Err(_) => {
                    hardware = None;
                    encoder.force_intra_frame();
                    report("Software H.264 (hardware encoder failed)".into());
                }
            }
        }
        let pixels = {
            let guard = mailbox.0.lock().unwrap();
            let wait = if hardware.is_some() { 5 } else { 100 };
            let (mut guard, _) = mailbox
                .1
                .wait_timeout_while(guard, Duration::from_millis(wait), |m| {
                    m.latest.is_none() && !m.closed
                })
                .unwrap();
            if guard.closed {
                return Err("The shared source closed; sharing stopped.".into());
            }
            guard.latest.take()
        };
        let mut captured_at = Instant::now();
        if let Some(pixels) = pixels {
            if pixels.arrived.elapsed() > Duration::from_millis(250) {
                continue;
            }
            captured_at = pixels.arrived;
            let (w, h) = pixels.rgba.dimensions();
            let scale = (quality.width() as f64 / w as f64)
                .min(quality.height as f64 / h as f64)
                .min(1.0);
            let w = ((w as f64 * scale) as u32 & !1).max(2);
            let h = ((h as f64 * scale) as u32 & !1).max(2);
            let rgba = if pixels.rgba.dimensions() == (w, h) {
                pixels.rgba
            } else {
                image::imageops::resize(&pixels.rgba, w, h, image::imageops::FilterType::Triangle)
            };
            let dimensions = (w as usize, h as usize);
            if configured != Some(dimensions) {
                hardware =
                    super::hardware::HardwareEncoder::new(dimensions.0, dimensions.1, quality).ok();
                report(
                    hardware
                        .as_ref()
                        .map(|h| format!("Hardware H.264: {}", h.name))
                        .unwrap_or_else(|| {
                            "Software H.264 (hardware unavailable for this resolution)".into()
                        }),
                );
                configured = Some(dimensions);
                keyframe = Instant::now() - Duration::from_secs(1);
            }
            let buffer = yuv.get_or_insert_with(|| YUVBuffer::new(dimensions.0, dimensions.1));
            if buffer.dimensions() != dimensions {
                *buffer = YUVBuffer::new(dimensions.0, dimensions.1);
                encoder.force_intra_frame();
            }
            buffer.read_rgba8(RgbaSliceU8::new(&rgba, dimensions));
            mailbox.0.lock().unwrap().spare = rgba.into_raw();
        } else if keyframe.elapsed() < Duration::from_secs(1) {
            continue;
        }
        let Some(yuv) = &yuv else {
            continue;
        };
        // Wall time, not a frame counter. Static sources also refresh their last
        // frame once per second for late joiners/loss recovery, without recapture.
        if keyframe.elapsed() >= Duration::from_secs(1) {
            encoder.force_intra_frame();
            if let Some(hw) = &hardware {
                let _ = hw.force_keyframe();
            }
            keyframe = Instant::now();
        }
        let micros = captured_at.saturating_duration_since(started).as_micros() as u64;
        if let Some(hw) = hardware.as_mut() {
            match hw.encode(yuv, micros) {
                Ok(frames) => {
                    if !send_frames(
                        frames,
                        started,
                        &tx,
                        &mut sequence,
                        &mut waiting_for_keyframe,
                    )? {
                        let _ = hw.force_keyframe();
                    }
                    continue;
                }
                Err(_) => {
                    hardware = None;
                    encoder.force_intra_frame();
                    report("Software H.264 (hardware encoder failed)".into());
                }
            }
        }
        let data = encoder
            .encode(yuv)
            .map_err(|_| "Screen encoding failed")?
            .to_vec();
        if data.is_empty() {
            continue;
        }
        if data.len() > MAX_FRAME_BYTES {
            waiting_for_keyframe = true;
            encoder.force_intra_frame();
            continue;
        }
        if !send_frames(
            vec![super::hardware::Encoded {
                data,
                timestamp: micros,
            }],
            started,
            &tx,
            &mut sequence,
            &mut waiting_for_keyframe,
        )? {
            encoder.force_intra_frame();
        }
    }
    Ok(())
}

fn send_frames(
    frames: Vec<super::hardware::Encoded>,
    started: Instant,
    tx: &tokio::sync::mpsc::Sender<(Instant, Vec<rtc::rtp::Packet>)>,
    sequence: &mut u16,
    waiting_for_keyframe: &mut bool,
) -> Result<bool, String> {
    let mut complete = true;
    for frame in frames {
        if frame.data.is_empty() {
            continue;
        }
        let at = started
            .checked_add(Duration::from_micros(frame.timestamp))
            .ok_or("Invalid video timestamp")?;
        if at.elapsed() > Duration::from_millis(250) {
            *waiting_for_keyframe = true;
            complete = false;
            continue;
        }
        if *waiting_for_keyframe && !super::recovery_frame(&frame.data) {
            complete = false;
            continue;
        }
        let packets = super::packetize(frame.data, sequence, (frame.timestamp * 90 / 1000) as u32)?;
        if tx.try_send((at, packets)).is_err() {
            *waiting_for_keyframe = true;
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
        let frame = |key| super::super::hardware::Encoded {
            data: if key {
                vec![0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
            } else {
                vec![0, 0, 1, 0x41, 1]
            },
            timestamp: 0,
        };
        assert!(send_frames(vec![frame(true)], started, &tx, &mut sequence, &mut waiting).unwrap());
        assert!(
            !send_frames(
                vec![frame(false)],
                started,
                &tx,
                &mut sequence,
                &mut waiting
            )
            .unwrap()
        );
        rx.try_recv().unwrap();
        assert!(
            !send_frames(
                vec![frame(false)],
                started,
                &tx,
                &mut sequence,
                &mut waiting
            )
            .unwrap()
        );
        assert!(
            rx.try_recv().is_err(),
            "dependent hardware output must not follow a dropped frame"
        );
        assert!(send_frames(vec![frame(true)], started, &tx, &mut sequence, &mut waiting).unwrap());
        assert!(!waiting);
        assert!(rx.try_recv().is_ok());
    }
}
