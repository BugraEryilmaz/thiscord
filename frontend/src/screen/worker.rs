//! Platform-neutral layer encoder loop. Each OS supplies GPU-resident capture
//! frames (D3D11 textures, IOSurface-backed CVPixelBuffers or GStreamer GPU
//! buffers) and a hardware H.264 encoder. Capture cadence, demand pause,
//! adaptive quality, keyframe/refresh policy, raw-input backpressure and paced
//! sender admission are identical on every platform.
use super::{
    adaptation::{Demand, QualityController},
    encode::{Encoded, EncoderOptions, SpeedPolicy},
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
use thiscord_shared::screen::Quality;

/// Source validity (closed/minimized window) is a syscall; 100 ms is ample.
pub const SOURCE_CHECK: Duration = Duration::from_millis(100);
/// Longest encoder-worker sleep without a capture or encoder wake. It bounds how
/// long atomics (demand, feedback, stop) can wait to be observed.
const IDLE_WAIT: Duration = Duration::from_millis(20);
/// Short poll while a raw frame waits on sender backpressure or send credit,
/// which have no wake. A frame waiting only for its cadence instead sleeps until
/// it is due; captures and encoder output still wake the worker early.
const PENDING_WAIT: Duration = Duration::from_millis(2);
/// Raw captures older than this are never encoded.
const RAW_MAX_AGE: Duration = Duration::from_millis(250);

/// One captured GPU image. `frame` is a cheap reference-counted handle.
#[derive(Clone)]
pub struct Pixels<F> {
    pub frame: F,
    pub width: u32,
    pub height: u32,
    pub arrived: Instant,
}
pub struct Mailbox<F> {
    pub latest: Option<Pixels<F>>,
    pub closed: bool,
    pub encoder_event: bool,
}
impl<F> Default for Mailbox<F> {
    fn default() -> Self {
        Self {
            latest: None,
            closed: false,
            encoder_event: false,
        }
    }
}
/// Latest-frame mailbox: overload replaces raw frames before they become
/// H.264 reference dependencies.
pub type Inbox<F> = Arc<(Mutex<Mailbox<F>>, Condvar)>;
pub fn inbox<F>() -> Inbox<F> {
    Arc::new((Mutex::new(Mailbox::default()), Condvar::new()))
}
/// Publish one capture to every layer mailbox; never blocks on encoders.
pub fn deliver<F: Clone>(targets: &[Inbox<F>], pixels: Pixels<F>, metrics: &Metrics) {
    for target in targets {
        let mut mailbox = target.0.lock().unwrap();
        if mailbox.latest.is_some() {
            metrics.add("raw_replaced", 1);
        }
        mailbox.latest = Some(pixels.clone());
        target.1.notify_one();
    }
}
/// The source ended (window closed, display removed, stream stopped).
pub fn close<F>(targets: &[Inbox<F>]) {
    for target in targets {
        target.0.lock().unwrap().closed = true;
        target.1.notify_all();
    }
}
/// Wakes the worker when asynchronous encoder output or input credit arrives.
pub fn waker<F: Send + 'static>(target: &Inbox<F>) -> Box<dyn Fn() + Send + Sync> {
    let target = target.clone();
    Box::new(move || {
        target.0.lock().unwrap().encoder_event = true;
        target.1.notify_one();
    })
}

/// Each worker publishes only its own committed cadence. A paused or slower
/// primary must not throttle a still-active low layer (and vice versa).
pub fn capture_fps(active_fps: &[AtomicU32; 2]) -> u32 {
    active_fps
        .iter()
        .map(|fps| fps.load(Ordering::Acquire))
        .max()
        .unwrap_or(0)
        .max(1)
}

/// A hardware encoder session for one layer at one fixed size/profile.
pub trait LayerEncoder {
    type Frame;
    /// Converted (scaled/NV12) input retained for static refreshes.
    type Input: Clone;
    fn name(&self) -> &str;
    /// "constrained_high", "high" or "baseline" as accepted by the encoder.
    fn profile(&self) -> &'static str;
    /// Submitted inputs whose output has not been retrieved.
    fn pending(&self) -> usize;
    /// Whether one more input can be submitted now.
    fn ready(&self) -> bool;
    /// Nonblocking retrieval of finished access units.
    fn poll(&mut self) -> Result<Vec<Encoded>, String>;
    /// GPU scale/color conversion. `None` means the conversion pool is busy.
    fn convert(&mut self, pixels: &Pixels<Self::Frame>) -> Result<Option<Self::Input>, String>;
    fn force_keyframe(&mut self) -> Result<(), String>;
    /// Submit input stamped in microseconds since `started`.
    fn encode(&mut self, input: &Self::Input, micros: u64) -> Result<Vec<Encoded>, String>;
    /// Current speed hint (0 = fastest) when the encoder exposes one.
    fn quality_vs_speed(&self) -> Option<u32>;
    /// Live speed-hint change. An error makes the worker rebuild the encoder,
    /// which then receives the fastest hint through `EncoderOptions`.
    fn set_quality_vs_speed(&mut self, value: u32) -> Result<(), String>;
    /// Human-readable pipeline for the share status line.
    fn describe(&self) -> String;
}

/// Per-layer platform hooks around one capture session.
pub trait Platform {
    type Frame: Clone + Send + 'static;
    type Encoder: LayerEncoder<Frame = Self::Frame>;
    /// Polled every `SOURCE_CHECK`; true stops sharing (closed/minimized source).
    fn source_lost(&mut self) -> bool {
        false
    }
    /// Called every iteration. `Ok(true)` ends this capture session so the
    /// caller can restart it in another format (Windows SDR -> HDR).
    fn tick(&mut self, _metrics: &Metrics, _secondary: bool) -> Result<bool, String> {
        Ok(false)
    }
    /// Creates an encoder for `size`, labelling platform-specific diagnostics.
    fn encoder(
        &mut self,
        pixels: &Pixels<Self::Frame>,
        size: (u32, u32),
        quality: Quality,
        options: EncoderOptions,
        wake: Box<dyn Fn() + Send + Sync>,
        metrics: &Metrics,
    ) -> Result<Self::Encoder, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stopped,
    /// The platform asked to restart capture (for example SDR -> HDR).
    Restart,
}
/// The user's stop ends sharing; a session stop only ends this capture format.
pub struct Stops<'a> {
    pub user: &'a AtomicBool,
    pub session: &'a AtomicBool,
}
impl Stops<'_> {
    pub fn stopped(&self) -> bool {
        self.user.load(Ordering::Acquire) || self.session.load(Ordering::Acquire)
    }
}
/// Inputs shared by both layers of one share.
pub struct Shared<'a> {
    pub stops: &'a Stops<'a>,
    pub connection: &'a Connection,
    pub started: Instant,
    pub active_fps: &'a [AtomicU32; 2],
    /// One stream-wide flag for both layers (see `VideoProfile`).
    pub high_profile: &'a AtomicBool,
}
/// One encoder layer's outputs and feedback.
pub struct Layer<'a> {
    pub quality: Quality,
    pub tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    pub metrics: &'a Metrics,
    pub force_keyframe: &'a AtomicBool,
    pub demand: &'a Demand,
    pub secondary: bool,
}

/// Runs the primary layer on this thread and the optional low layer on a
/// scoped thread. Any low-layer failure ends the share; a primary restart
/// request ends only this session.
#[allow(clippy::too_many_arguments)]
pub fn run_layers<P, L>(
    primary: &mut P,
    mailbox: &Inbox<P::Frame>,
    finished: Option<&dyn Fn() -> bool>,
    low: Option<(L, Inbox<L::Frame>, &super::LayerOutput)>,
    shared: &Shared,
    layer: Layer,
    stop: &AtomicBool,
    report: &mut dyn FnMut(String),
) -> Result<Outcome, String>
where
    P: Platform,
    L: Platform + Send,
    <L::Encoder as LayerEncoder>::Input: Send,
{
    let low_quality = thiscord_shared::screen::LayerTargets::low_quality(layer.quality);
    std::thread::scope(|scope| {
        let low_job = low.map(|(mut platform, mailbox, low)| {
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
                let _stop_on_exit = StopOnExit(shared.stops);
                let result = encode(
                    &mut platform,
                    &mailbox,
                    None,
                    shared,
                    Layer {
                        quality: low_quality,
                        tx,
                        metrics: &low.metrics,
                        force_keyframe: &low.force_keyframe,
                        demand: &low.demand,
                        secondary: true,
                    },
                    &mut |_| {},
                );
                if result.is_err() {
                    shared.stops.user.store(true, Ordering::Release);
                }
                result
            })
        });
        let result = encode(primary, mailbox, finished, shared, layer, report);
        // A format restart only ends this session; anything else ends sharing.
        if result != Ok(Outcome::Restart) {
            stop.store(true, Ordering::Release);
        }
        shared.stops.session.store(true, Ordering::Release);
        let secondary = low_job.map_or(Ok(Outcome::Stopped), |job| {
            job.join()
                .unwrap_or_else(|_| Err("Screen low encoder worker failed".into()))
        });
        result.and_then(|outcome| secondary.map(|_| outcome))
    })
}

/// The layer worker: owns one encoder, rebuilt on quality/profile changes.
pub fn encode<P: Platform>(
    platform: &mut P,
    mailbox: &Inbox<P::Frame>,
    finished: Option<&dyn Fn() -> bool>,
    shared: &Shared,
    layer: Layer,
    report: &mut dyn FnMut(String),
) -> Result<Outcome, String> {
    let Layer {
        quality,
        tx,
        metrics,
        force_keyframe,
        demand,
        secondary,
    } = layer;
    let Shared {
        stops,
        connection,
        started,
        active_fps,
        high_profile,
    } = *shared;
    metrics.label(
        "encoder_thread_priority",
        super::priority::promote_video_thread(),
    );
    let requested = quality;
    let mut adaptation = QualityController::new(requested);
    let mut quality = requested.constrained(demand.bitrate());
    let mut high = high_profile.load(Ordering::Acquire);
    let mut paused = true;
    let mut reconfigure = true;
    let mut last_raw: Option<Pixels<P::Frame>> = None;
    let mut refresh_after = Instant::now();
    let mut input_after = Instant::now();
    let mut encoder: Option<P::Encoder> = None;
    let mut configured = None;
    let mut last_input: Option<<P::Encoder as LayerEncoder>::Input> = None;
    let mut pending_pixels: Option<Pixels<P::Frame>> = None;
    // Set when `pending_pixels` waits only for time, not for backpressure.
    let mut pending_due: Option<Instant> = None;
    let mut sequence = 0;
    let mut waiting_for_keyframe = true;
    let mut keyframe = KeyframePolicy::default();
    let mut speed = SpeedPolicy::default();
    let mut previous = Instant::now();
    let mut previous_bitrate = None;
    let mut source_checked = Instant::now() - SOURCE_CHECK;
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
        if source_checked.elapsed() >= SOURCE_CHECK {
            source_checked = Instant::now();
            if platform.source_lost() {
                return Err(
                    "The shared source closed or became unavailable; sharing stopped.".into(),
                );
            }
        }
        if mailbox.0.lock().unwrap().closed || finished.is_some_and(|f| f()) {
            return Err("The shared source closed or became unavailable; sharing stopped.".into());
        }
        if platform.tick(metrics, secondary)? {
            return Ok(Outcome::Restart);
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
        let Some(next) = next else {
            active_fps[usize::from(secondary)].store(0, Ordering::Release);
            if !paused {
                encoder = None;
                configured = None;
                last_input = None;
                waiting_for_keyframe = true;
                metrics.label("video_paused", true);
            }
            paused = true;
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
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
            encoder = None;
            configured = None;
            last_input = None;
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
            // Sleep until real work can be due: a capture/encoder notification
            // wakes the condvar early. Pending input polls briefly for time,
            // credit or sender drain; otherwise wait for the next static refresh.
            let timeout = if pending_pixels.is_some() {
                pending_due.map_or(PENDING_WAIT, |at| {
                    at.saturating_duration_since(now).min(IDLE_WAIT)
                })
            } else if encoder.is_some() && last_input.is_some() {
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
            metrics.add("worker_wakes", 1);
            if guard.closed {
                return Err("The shared source closed; sharing stopped.".into());
            }
            // Consume the old pending frame even when a newer capture wins;
            // retaining it would submit an older timestamp after the new frame.
            let pending = pending_pixels.take();
            if guard.latest.is_some() && pending.is_some() {
                metrics.add("raw_replaced", 1);
            }
            let fresh = guard.latest.is_some();
            (guard.latest.take().or(pending), fresh)
        };
        // Capture waiting can outlast an encoder output event. Drain after
        // waking, before testing encoder readiness for this new frame.
        let mut rebuild = false;
        if let Some(hw) = encoder.as_mut() {
            metrics.peak("encoder_peak_pending", hw.pending() as u64);
            metrics.peak(
                "sender_peak_frames",
                (tx.max_capacity() - tx.capacity()) as u64,
            );
            let frames = hw.poll().map_err(|e| format!("GPU encoder failed: {e}"))?;
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
            encoder = None;
            configured = None;
            last_input = None;
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
        let now = Instant::now();
        if pixels.is_none() && now < refresh_after {
            pending_due = Some(refresh_after);
            pending_pixels = pixels;
            continue;
        }
        if capture_fps(active_fps) > quality.fps && now < input_after {
            pending_due = Some(input_after);
            pending_pixels = pixels;
            continue;
        }
        pending_due = None;
        let mut captured_at = Instant::now();
        let queued = tx.max_capacity() - tx.capacity();
        if !super::encoder_has_capacity(
            super::sender_queue_frames(quality.fps).saturating_sub(queued),
            encoder.as_ref().map_or(0, |hw| hw.pending()),
        ) || !send_budget.has_capacity()
        {
            // Dropping raw input preserves the H.264 dependency chain. Poll
            // hardware above even when congested, but submit no new surfaces.
            if fresh {
                metrics.add("encode_backpressure", 1);
            }
            // Retain the latest raw frame so a source that becomes static
            // during backpressure still delivers its final update. New capture
            // replaces this frame before it becomes an H.264 dependency.
            pending_pixels = pixels;
            continue;
        }
        if let Some(pixels) = pixels {
            if pixels.arrived.elapsed() > RAW_MAX_AGE {
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
            let (w, h) = encoded_size(quality, recovery_scale, (pixels.width, pixels.height));
            if configured != Some((w, h)) {
                let options = EncoderOptions {
                    high_profile: high,
                    quality_speed: speed.hint(w as usize, h as usize, quality.fps),
                };
                speed.reset_window();
                // Release the previous hardware session before opening another.
                drop(encoder.take());
                let created =
                    platform.encoder(&pixels, (w, h), quality, options, waker(mailbox), metrics)?;
                metrics.label("resolution", format!("{w}x{h}"));
                metrics.label("target_fps", quality.fps);
                metrics.label("target_bitrate", quality.bitrate());
                metrics.label("encoder", created.name());
                metrics.label("encoder_profile", created.profile());
                metrics.label(
                    "encoder_gop_target_frames",
                    super::keyframe::gop_frames(quality.fps),
                );
                metrics.label(
                    "encoder_quality_vs_speed",
                    created
                        .quality_vs_speed()
                        .map_or_else(|| "unavailable".into(), |v| v.to_string()),
                );
                report(created.describe());
                encoder = Some(created);
                configured = Some((w, h));
                last_input = None;
                keyframe = KeyframePolicy::default();
                force_keyframe.store(true, Ordering::Release);
            }
            let hw = encoder.as_mut().expect("encoder configured above");
            if !hw.ready() {
                metrics.add("gpu_busy", 1);
                metrics.add("encoder_input_busy", 1);
                pending_pixels = Some(pixels);
                continue;
            }
            let at = Instant::now();
            let converted = hw.convert(&pixels)?;
            metrics.time("gpu_submit", at.elapsed());
            let Some(converted) = converted else {
                metrics.add("gpu_busy", 1);
                metrics.add("conversion_surface_busy", 1);
                continue;
            };
            last_input = Some(converted);
        } else if !keyframe.refresh_due(Instant::now(), force_keyframe.load(Ordering::Acquire)) {
            continue;
        }
        let (Some(hw), Some(input)) = (&mut encoder, &last_input) else {
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
                .map_err(|e| format!("GPU keyframe request failed: {e}"))?;
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
            .encode(input, micros)
            .map_err(|e| format!("GPU encode failed: {e}"))?;
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
            encoder = None;
            configured = None;
            last_input = None;
            waiting_for_keyframe = true;
            reconfigure = true;
            force_keyframe.store(true, Ordering::Release);
        }
    }
    Ok(Outcome::Stopped)
}

/// Aspect-preserving encoded size: never upscales, even dimensions.
pub fn encoded_size(quality: Quality, recovery_scale: u32, source: (u32, u32)) -> (u32, u32) {
    let scale = (quality.width() as f64 * f64::from(recovery_scale) / 100.0 / source.0 as f64)
        .min(quality.height as f64 * f64::from(recovery_scale) / 100.0 / source.1 as f64)
        .min(1.0);
    let w = ((source.0 as f64 * scale) as u32 & !1).max(2);
    let h = ((source.1 as f64 * scale) as u32 & !1).max(2);
    (w, h)
}

/// Feed encoder service times to the speed policy. Returns true when the encoder
/// must be rebuilt because it ignored a live speed-hint change.
fn observe_speed<E: LayerEncoder>(
    frames: &[Encoded],
    speed: &mut SpeedPolicy,
    hw: &mut E,
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
fn record_frames(frames: &[Encoded], started: Instant, metrics: &Metrics) -> bool {
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
pub(crate) fn send_frames(
    frames: Vec<Encoded>,
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
    fn encoded_size_preserves_aspect_without_upscaling() {
        let q = Quality {
            height: 1080,
            fps: 30,
        };
        assert_eq!(encoded_size(q, 100, (3840, 2160)), (1920, 1080));
        assert_eq!(encoded_size(q, 100, (1280, 720)), (1280, 720));
        assert_eq!(encoded_size(q, 100, (5120, 2880)), (1920, 1080));
        // Ultrawide sources are bounded by width; odd sizes round down to even.
        assert_eq!(encoded_size(q, 100, (3440, 1440)), (1920, 802));
        assert_eq!(encoded_size(q, 50, (3840, 2160)), (960, 540));
        assert_eq!(encoded_size(q, 100, (801, 601)), (800, 600));
    }

    /// Synthetic platform: frames are integers; the encoder emits an IDR for
    /// every forced input and a delta otherwise, completing synchronously.
    struct Fake {
        created: Arc<AtomicU32>,
    }
    struct FakeEncoder {
        force: bool,
    }
    impl LayerEncoder for FakeEncoder {
        type Frame = u32;
        type Input = u32;
        fn name(&self) -> &str {
            "fake"
        }
        fn profile(&self) -> &'static str {
            "baseline"
        }
        fn pending(&self) -> usize {
            0
        }
        fn ready(&self) -> bool {
            true
        }
        fn poll(&mut self) -> Result<Vec<Encoded>, String> {
            Ok(Vec::new())
        }
        fn convert(&mut self, pixels: &Pixels<u32>) -> Result<Option<u32>, String> {
            Ok(Some(pixels.frame))
        }
        fn force_keyframe(&mut self) -> Result<(), String> {
            self.force = true;
            Ok(())
        }
        fn encode(&mut self, _: &u32, micros: u64) -> Result<Vec<Encoded>, String> {
            let data = if std::mem::take(&mut self.force) {
                vec![0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
            } else {
                vec![0, 0, 1, 0x41, 1]
            };
            Ok(vec![Encoded {
                data,
                timestamp: micros,
                encoding_duration: Duration::from_micros(100),
            }])
        }
        fn quality_vs_speed(&self) -> Option<u32> {
            None
        }
        fn set_quality_vs_speed(&mut self, _: u32) -> Result<(), String> {
            Ok(())
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }
    impl Platform for Fake {
        type Frame = u32;
        type Encoder = FakeEncoder;
        fn encoder(
            &mut self,
            _: &Pixels<u32>,
            _: (u32, u32),
            _: Quality,
            _: EncoderOptions,
            _: Box<dyn Fn() + Send + Sync>,
            _: &Metrics,
        ) -> Result<FakeEncoder, String> {
            self.created.fetch_add(1, Ordering::AcqRel);
            Ok(FakeEncoder { force: false })
        }
    }

    #[test]
    fn worker_starts_with_idr_pauses_without_demand_and_stops_on_close() {
        let connection = Connection::default();
        let (user, session) = (AtomicBool::new(false), AtomicBool::new(false));
        let stops = Stops {
            user: &user,
            session: &session,
        };
        let active_fps = [AtomicU32::new(0), AtomicU32::new(0)];
        let high = AtomicBool::new(false);
        let shared = Shared {
            stops: &stops,
            connection: &connection,
            started: Instant::now(),
            active_fps: &active_fps,
            high_profile: &high,
        };
        let demand = Demand::default();
        demand.update(5_000_000);
        let force = AtomicBool::new(false);
        let metrics = Metrics::default();
        let mailbox = inbox::<u32>();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let created = Arc::new(AtomicU32::new(0));
        let mut platform = Fake {
            created: created.clone(),
        };
        let quality = Quality {
            height: 720,
            fps: 30,
        };
        std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                encode(
                    &mut platform,
                    &mailbox,
                    None,
                    &shared,
                    Layer {
                        quality,
                        tx,
                        metrics: &metrics,
                        force_keyframe: &force,
                        demand: &demand,
                        secondary: false,
                    },
                    &mut |_| {},
                )
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                while rx.try_recv().is_ok() {}
                deliver(
                    std::slice::from_ref(&mailbox),
                    Pixels {
                        frame: 1,
                        width: 1280,
                        height: 720,
                        arrived: Instant::now(),
                    },
                    &metrics,
                );
                std::thread::sleep(Duration::from_millis(40));
                if metrics.snapshot().counters.get("encoded_frames").copied() >= Some(3) {
                    break;
                }
            }
            close(std::slice::from_ref(&mailbox));
            let result = job.join().unwrap();
            assert!(result.is_err(), "closing the source ends sharing");
        });
        let snapshot = metrics.snapshot();
        assert!(snapshot.counters["encoded_frames"] >= 3);
        assert!(
            snapshot.counters["keyframes"] >= 1,
            "first output is an IDR"
        );
        assert_eq!(created.load(Ordering::Acquire), 1);
    }

    /// A 15 fps low layer fed 60 fps captures holds most frames for cadence.
    /// It must sleep until the next frame is due, not poll every 2 ms.
    #[test]
    fn cadence_held_frames_sleep_until_due_instead_of_polling() {
        let connection = Connection::default();
        let (user, session) = (AtomicBool::new(false), AtomicBool::new(false));
        let stops = Stops {
            user: &user,
            session: &session,
        };
        let active_fps = [AtomicU32::new(60), AtomicU32::new(15)];
        let high = AtomicBool::new(false);
        let shared = Shared {
            stops: &stops,
            connection: &connection,
            started: Instant::now(),
            active_fps: &active_fps,
            high_profile: &high,
        };
        let demand = Demand::default();
        demand.update(5_000_000);
        let force = AtomicBool::new(false);
        let metrics = Metrics::default();
        let mailbox = inbox::<u32>();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut platform = Fake {
            created: Arc::default(),
        };
        let quality = Quality {
            height: 720,
            fps: 15,
        };
        let run = Duration::from_secs(1);
        std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                encode(
                    &mut platform,
                    &mailbox,
                    None,
                    &shared,
                    Layer {
                        quality,
                        tx,
                        metrics: &metrics,
                        force_keyframe: &force,
                        demand: &demand,
                        secondary: true,
                    },
                    &mut |_| {},
                )
            });
            let start = Instant::now();
            let mut frame = 0;
            while start.elapsed() < run {
                while rx.try_recv().is_ok() {}
                frame += 1;
                deliver(
                    std::slice::from_ref(&mailbox),
                    Pixels {
                        frame,
                        width: 1280,
                        height: 720,
                        arrived: Instant::now(),
                    },
                    &metrics,
                );
                std::thread::sleep(Duration::from_micros(16_667));
            }
            close(std::slice::from_ref(&mailbox));
            assert!(job.join().unwrap().is_err());
        });
        let counters = metrics.snapshot().counters;
        let (wakes, encoded) = (counters["worker_wakes"], counters["encoded_frames"]);
        assert!(
            (8..=20).contains(&encoded),
            "encodes at the layer rate: {encoded}"
        );
        // Each ~60 Hz capture wakes the worker once (about 61 measured). The
        // former 2 ms poll measured 105 on Windows, whose default timer rounds
        // short waits up, and several hundred where 2 ms sleeps are honored.
        assert!(wakes < 95, "{wakes} worker wakes in one second");
    }

    #[test]
    fn encoded_backpressure_discards_pending_deltas_until_a_recovery_frame() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let started = Instant::now();
        let mut sequence = 0;
        let mut waiting = true;
        let metrics = Metrics::default();
        let send_budget = super::super::pacing::SendBudget::new(21_600_000);
        let frame = |key| Encoded {
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
