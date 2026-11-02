//! macOS screen sharing: ScreenCaptureKit delivers compositor frames as
//! IOSurface-backed NV12 (BT.709 video range) buffers, already scaled to the
//! selected preset by the window server. VideoToolbox encodes those surfaces
//! on the media engine; any further adaptive downscale happens inside the
//! hardware session. No pixel readback, CPU conversion or software encoder.
//! Encode admission, cadence, layers and pacing are shared with Windows via
//! `super::worker`.
mod audio;
mod content;
mod encoder;
#[cfg(test)]
mod tests;

pub use audio::system_audio;
pub use content::sources;

use super::{
    cadence::Cadence,
    metrics::Metrics,
    worker::{self, Inbox, Layer, Outcome, Pixels, Shared, Stops},
};
use crate::audio::connection::Connection;
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, ProtocolObject},
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};
use objc2_foundation::{NSArray, NSError, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCFrameStatus, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{Quality, SourceId};

/// One retained IOSurface-backed capture buffer; cloning only retains it.
#[derive(Clone)]
pub struct Frame(CFRetained<CVPixelBuffer>);
// SAFETY: CVPixelBuffer is a reference-counted CoreFoundation object whose
// retain/release is thread-safe. The buffer is never written after capture:
// ScreenCaptureKit hands it over complete and VideoToolbox only reads it.
unsafe impl Send for Frame {}
// SAFETY: as above; shared access is read-only.
unsafe impl Sync for Frame {}
impl Frame {
    pub(crate) fn new(buffer: CFRetained<CVPixelBuffer>) -> Self {
        Self(buffer)
    }
    pub(crate) fn buffer(&self) -> &CVPixelBuffer {
        &self.0
    }
}

/// `Retained` ScreenCaptureKit objects are immutable snapshots that Apple
/// documents as usable from any queue; this only moves them between threads.
pub(crate) struct Sendable<T>(pub T);
// SAFETY: see the type documentation; no interior mutation through `T`.
unsafe impl<T> Send for Sendable<T> {}

/// SCK (12.3+) availability; also guards weakly linked framework symbols.
pub(crate) fn at_least(major: isize, minor: isize) -> bool {
    objc2_foundation::NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(
        objc2_foundation::NSOperatingSystemVersion {
            majorVersion: major,
            minorVersion: minor,
            patchVersion: 0,
        },
    )
}
pub(crate) fn require_capture() -> Result<(), String> {
    if !at_least(12, 3) {
        return Err("Screen sharing requires macOS 12.3 or newer.".into());
    }
    content::require_permission()
}

/// Waits for an Objective-C completion handler with a bounded timeout.
pub(crate) fn wait_completion(
    start: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
    timeout: Duration,
    what: &str,
) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Option<String>>(1);
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: SCK passes either null or a valid NSError for this call.
        let message = unsafe { error.as_ref() }.map(content::describe_error);
        let _ = tx.try_send(message);
    });
    start(&block);
    match rx.recv_timeout(timeout) {
        Ok(None) => Ok(()),
        Ok(Some(error)) => Err(format!("{what}: {error}")),
        Err(_) => Err(format!("{what}: timed out")),
    }
}

/// Maps ScreenCaptureKit host-clock presentation times to `Instant`.
#[derive(Clone, Copy)]
pub(crate) struct HostClock {
    seconds: f64,
    instant: Instant,
}
impl HostClock {
    pub(crate) fn now() -> Self {
        // SAFETY: the host time clock is a process-wide singleton.
        let seconds = unsafe {
            objc2_core_media::CMClock::host_time_clock()
                .time()
                .seconds()
        };
        Self {
            seconds,
            instant: Instant::now(),
        }
    }
    pub(crate) fn instant(&self, time: CMTime) -> Option<Instant> {
        // SAFETY: plain value conversion.
        let seconds = unsafe { time.seconds() };
        if !seconds.is_finite() {
            return None;
        }
        let delta = seconds - self.seconds;
        let offset = Duration::try_from_secs_f64(delta.abs()).ok()?;
        if delta >= 0.0 {
            self.instant.checked_add(offset)
        } else {
            self.instant.checked_sub(offset)
        }
        .map(|at| at.min(Instant::now()))
    }
}

struct VideoIvars {
    mailboxes: Vec<Inbox<Frame>>,
    stop: Arc<AtomicBool>,
    session: Arc<AtomicBool>,
    connection: Connection,
    active_fps: Arc<[AtomicU32; 2]>,
    metrics: Metrics,
    clock: HostClock,
    /// Touched only on the serial sample-handler queue; never contended.
    cadence: Mutex<(Duration, Cadence)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class does not
    // implement Drop. Ivars are thread-safe (atomics/Arc/uncontended Mutex).
    #[unsafe(super(NSObject))]
    #[name = "ThiscordScreenVideoOutput"]
    #[ivars = VideoIvars]
    struct VideoOutput;

    unsafe impl NSObjectProtocol for VideoOutput {}

    unsafe impl SCStreamOutput for VideoOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output_sample(
            &self,
            stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.frame(stream, sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for VideoOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.ivars().metrics.event(&format!(
                "capture stopped: {}",
                content::describe_error(error)
            ));
            worker::close(&self.ivars().mailboxes);
        }
    }
);

impl VideoOutput {
    fn new(ivars: VideoIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
    fn frame(&self, _stream: &SCStream, sample: &CMSampleBuffer) {
        let ivars = self.ivars();
        if ivars.stop.load(Ordering::Acquire)
            || ivars.session.load(Ordering::Acquire)
            || !ivars.connection.active()
        {
            return;
        }
        ivars.metrics.add("capture_events", 1);
        // Idle/blank frames carry no new image; static sources are refreshed
        // from the held buffer by the encoder worker.
        if frame_status(sample) != Some(SCFrameStatus::Complete) {
            ivars.metrics.add("capture_idle", 1);
            return;
        }
        let now = Instant::now();
        // SAFETY: valid sample buffer for the duration of this callback.
        let pts = unsafe { sample.presentation_time_stamp() };
        // Compositor time drives RTP timing. A mapping that is implausibly
        // old (clock mismatch) would expire every frame; fall back to arrival.
        let captured_at = match ivars.clock.instant(pts) {
            Some(at) if now.saturating_duration_since(at) <= Duration::from_millis(100) => at,
            _ => {
                ivars.metrics.add("capture_clock_fallback", 1);
                now
            }
        };
        let accepted = {
            let Ok(mut cadence) = ivars.cadence.try_lock() else {
                return;
            };
            let fps = worker::capture_fps(&ivars.active_fps);
            let period = Duration::from_secs_f64(1.0 / f64::from(fps));
            if period != cadence.0 {
                *cadence = (period, Cadence::new(period));
            }
            cadence.1.accept(captured_at)
        };
        if !accepted {
            ivars.metrics.add("capture_throttled", 1);
            return;
        }
        // SAFETY: valid sample buffer; the image buffer is retained.
        let Some(buffer) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        let (width, height) = (
            CVPixelBufferGetWidth(&buffer) as u32,
            CVPixelBufferGetHeight(&buffer) as u32,
        );
        if !(2..=16384).contains(&width) || !(2..=16384).contains(&height) {
            ivars.metrics.add("capture_invalid", 1);
            return;
        }
        worker::deliver(
            &ivars.mailboxes,
            Pixels {
                frame: Frame::new(buffer),
                width,
                height,
                arrived: captured_at,
            },
            &ivars.metrics,
        );
        ivars.metrics.time("capture_submit", now.elapsed());
    }
}

/// `SCStreamFrameInfoStatus` from the first sample attachment dictionary.
fn frame_status(sample: &CMSampleBuffer) -> Option<SCFrameStatus> {
    use objc2_core_foundation::{CFDictionary, CFNumber};
    // SAFETY: valid sample; attachments are a CFArray of CFDictionary.
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    if attachments.count() < 1 {
        return None;
    }
    // SAFETY: index 0 is in bounds; elements are CFDictionaryRef owned by the array.
    let dict = unsafe { attachments.value_at_index(0) }.cast::<CFDictionary>();
    // SAFETY: SCStreamFrameInfoStatus is a constant NSString (toll-free CFString).
    let key: *const objc2_foundation::NSString = unsafe { SCStreamFrameInfoStatus };
    // SAFETY: non-null dictionary checked; the value is a CFNumber or null.
    let value = unsafe { dict.as_ref()?.value(key.cast()) }.cast::<CFNumber>();
    // SAFETY: checked non-null; owned by the dictionary for this callback.
    let raw = unsafe { value.as_ref()? }.as_isize()?;
    Some(SCFrameStatus(raw))
}

/// The capture session: one SCStream feeding both layer mailboxes.
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
    require_capture()?;
    let target = content::Target::resolve(source)?;
    let mailbox = worker::inbox();
    let active_fps: Arc<[AtomicU32; 2]> = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let low_mailbox = low.as_ref().map(|_| worker::inbox());
    let mut mailboxes = vec![mailbox.clone()];
    mailboxes.extend(low_mailbox.iter().cloned());
    let session_stop = Arc::new(AtomicBool::new(false));
    // The window server scales into the selected preset; never upscale.
    let (width, height) = worker::encoded_size(quality, 100, target.pixels);
    metrics.label("capture_format", "nv12_709");
    metrics.label(
        "capture_resolution",
        format!("{}x{}", target.pixels.0, target.pixels.1),
    );
    let period = Duration::from_secs_f64(1.0 / f64::from(quality.fps));
    let output = VideoOutput::new(VideoIvars {
        mailboxes,
        stop: stop.clone(),
        session: session_stop.clone(),
        connection: connection.clone(),
        active_fps: active_fps.clone(),
        metrics: metrics.clone(),
        clock: HostClock::now(),
        cadence: Mutex::new((period, Cadence::new(period))),
    });
    let config = video_config(width, height, quality.fps);
    let queue = DispatchQueue::new("com.thiscord.screen.capture", None);
    // SAFETY: valid filter/configuration; the delegate outlives the stream
    // because `output` is held until after capture stops below.
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &target.filter,
            &config,
            Some(ProtocolObject::from_ref(&*output)),
        )
    };
    // SAFETY: the output object implements SCStreamOutput and outlives the stream.
    unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*output),
            SCStreamOutputType::Screen,
            Some(&queue),
        )
    }
    .map_err(|e| {
        format!(
            "Cannot start screen capture: {}",
            content::describe_error(&e)
        )
    })?;
    wait_completion(
        // SAFETY: the stream stays alive until the completion handler returns.
        |done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) },
        Duration::from_secs(10),
        "Cannot start screen capture",
    )
    .map_err(content::permission_hint)?;
    let stops = Stops {
        user: &stop,
        session: &session_stop,
    };
    let shared = Shared {
        stops: &stops,
        connection: &connection,
        started,
        active_fps: &active_fps,
        high_profile: &high_profile,
    };
    let platform = || Sck {
        window: target.window,
    };
    let result = worker::run_layers(
        &mut platform(),
        &mailbox,
        None,
        low.as_ref()
            .zip(low_mailbox)
            .map(|(low, mailbox)| (platform(), mailbox, low)),
        &shared,
        Layer {
            quality,
            tx,
            metrics: &metrics,
            force_keyframe: &force_keyframe,
            demand: &demand,
            secondary: false,
        },
        &stop,
        &mut report,
    );
    stop.store(true, Ordering::Release);
    session_stop.store(true, Ordering::Release);
    let stopped = wait_completion(
        // SAFETY: the stream stays alive until the completion handler returns.
        |done| unsafe { stream.stopCaptureWithCompletionHandler(Some(done)) },
        Duration::from_secs(3),
        "Cannot stop screen capture",
    );
    drop(stream);
    drop(output);
    result.and_then(|outcome| {
        debug_assert_eq!(outcome, Outcome::Stopped);
        // A stream that ended on its own (window closed) already failed above.
        stopped.or(Ok(()))
    })
}

fn video_config(width: u32, height: u32, fps: u32) -> Retained<SCStreamConfiguration> {
    use objc2_core_graphics::{kCGColorSpaceITUR_709, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2};
    // SAFETY: plain property setters on a fresh configuration object.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width as usize);
        config.setHeight(height as usize);
        config.setPixelFormat(objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        config.setColorSpaceName(kCGColorSpaceITUR_709);
        config.setShowsCursor(true);
        config.setScalesToFit(true);
        // Eight surfaces cover the held refresh image, both mailboxes and
        // four in-flight hardware encodes before SCK must recycle a buffer.
        config.setQueueDepth(8);
        config.setMinimumFrameInterval(CMTime::new(1, fps.clamp(1, 60) as i32));
        config
    }
}

/// Per-layer hooks: window validity and VideoToolbox session creation.
struct Sck {
    window: Option<u32>,
}
impl worker::Platform for Sck {
    type Frame = Frame;
    type Encoder = encoder::VideoToolbox;
    fn source_lost(&mut self) -> bool {
        self.window.is_some_and(|id| !content::window_exists(id))
    }
    fn encoder(
        &mut self,
        _pixels: &Pixels<Frame>,
        size: (u32, u32),
        quality: Quality,
        options: super::encode::EncoderOptions,
        wake: Box<dyn Fn() + Send + Sync>,
        metrics: &Metrics,
    ) -> Result<encoder::VideoToolbox, String> {
        let encoder = encoder::VideoToolbox::new(size, quality, options, wake)?;
        metrics.label(
            "encoder_buffer_target_bytes",
            super::encode::buffer_bytes(quality.bitrate()),
        );
        metrics.label("encoder_buffer_applied", encoder.buffer_applied);
        metrics.label("encoder_gop_applied", encoder.gop_applied);
        metrics.label("encoder_low_latency", encoder.low_latency);
        metrics.label(
            "pixel_path",
            "SCK NV12 IOSurface / VideoToolbox scale / hardware H.264",
        );
        Ok(encoder)
    }
}

/// Whole-display filter excluding nothing (Thiscord's own windows included,
/// matching Windows Graphics Capture).
pub(crate) fn display_filter(
    display: &objc2_screen_capture_kit::SCDisplay,
) -> Retained<SCContentFilter> {
    // SAFETY: a valid display and an empty exclusion list.
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            display,
            &NSArray::new(),
        )
    }
}
