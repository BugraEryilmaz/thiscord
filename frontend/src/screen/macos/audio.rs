//! System audio through an audio-only ScreenCaptureKit stream on a display
//! filter: every application's output (even for a window share, matching
//! Windows), excluding Thiscord's own process. The SCK handler only copies
//! PCM into a preallocated scratch buffer and a bounded SPSC ring; Opus and
//! RTP run on the caller's worker thread.
use super::{content, wait_completion};
use crate::screen::metrics::Metrics;
use dispatch2::DispatchQueue;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, ProtocolObject},
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::{NSError, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const RATE: usize = 48_000;
/// Half a second of interleaved stereo between the SCK queue and the worker.
const RING: usize = RATE;
/// Largest SCK audio buffer accepted (frames per channel).
const MAX_FRAMES: usize = 8192;

#[repr(C)]
struct StreamDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}
const FLOAT: u32 = 1;
const NON_INTERLEAVED: u32 = 1 << 5;
unsafe extern "C-unwind" {
    fn CMAudioFormatDescriptionGetStreamBasicDescription(
        desc: NonNull<c_void>,
    ) -> *const StreamDescription;
}

struct Handler {
    producer: HeapProd<f32>,
    scratch: Vec<f32>,
}
struct AudioIvars {
    /// Touched only on the serial SCK queue; `try_lock` never waits.
    handler: Mutex<Handler>,
    packets: AtomicU64,
    overflow: AtomicU64,
    rejected: AtomicU64,
    closed: AtomicBool,
    worker: std::thread::Thread,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    // Ivars are atomics, a thread handle and an uncontended Mutex.
    #[unsafe(super(NSObject))]
    #[name = "ThiscordSystemAudioOutput"]
    #[ivars = AudioIvars]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output_sample(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Audio {
                self.audio(sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for AudioOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, _error: &NSError) {
            self.ivars().closed.store(true, Ordering::Release);
            self.ivars().worker.unpark();
        }
    }
);

impl AudioOutput {
    fn new(ivars: AudioIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
    fn audio(&self, sample: &CMSampleBuffer) {
        let ivars = self.ivars();
        ivars.packets.fetch_add(1, Ordering::Relaxed);
        let Ok(mut handler) = ivars.handler.try_lock() else {
            ivars.overflow.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match copy_interleaved(sample, &mut handler) {
            Some(true) => {}
            Some(false) => {
                ivars.overflow.fetch_add(1, Ordering::Relaxed);
                return;
            }
            None => {
                ivars.rejected.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        ivars.worker.unpark();
    }
}

/// Copies one SCK float buffer (planar or interleaved, mono or stereo) into
/// the ring as interleaved stereo. No allocation: `scratch` is preallocated.
/// `Some(false)` means the ring was full and the buffer was dropped whole.
fn copy_interleaved(sample: &CMSampleBuffer, handler: &mut Handler) -> Option<bool> {
    // SAFETY: valid sample for this callback; format/block are retained.
    let (format, block, frames) = unsafe {
        (
            sample.format_description()?,
            sample.data_buffer()?,
            sample.num_samples() as usize,
        )
    };
    // SAFETY: an audio format description; the result lives as long as it.
    let asbd = unsafe {
        CMAudioFormatDescriptionGetStreamBasicDescription(NonNull::from(&*format).cast())
            .as_ref()?
    };
    let channels = asbd.channels_per_frame as usize;
    if asbd.format_flags & FLOAT == 0
        || asbd.bits_per_channel != 32
        || !(1..=2).contains(&channels)
        || frames == 0
        || frames > MAX_FRAMES
        || (asbd.sample_rate - RATE as f64).abs() > 1.0
    {
        return None;
    }
    let samples = frames * channels;
    // SAFETY: plain length query.
    if unsafe { block.data_length() } < samples * 4 {
        return None;
    }
    let scratch = &mut handler.scratch[..samples];
    // SAFETY: `scratch` holds exactly `samples * 4` writable bytes.
    if unsafe { block.copy_data_bytes(0, samples * 4, NonNull::from(&mut *scratch).cast()) } != 0 {
        return None;
    }
    let planar = asbd.format_flags & NON_INTERLEAVED != 0;
    let at = |frame: usize, channel: usize| {
        let channel = channel.min(channels - 1);
        if planar {
            scratch[channel * frames + frame]
        } else {
            scratch[frame * channels + channel]
        }
    };
    if handler.producer.vacant_len() < frames * 2 {
        return Some(false);
    }
    for frame in 0..frames {
        let pair = [at(frame, 0), at(frame, 1)];
        handler.producer.push_slice(&pair);
    }
    Some(true)
}

/// Blocks until stop/disconnect/error, delivering interleaved 48 kHz stereo.
pub fn system_audio(
    stop: &AtomicBool,
    running: &dyn Fn() -> bool,
    metrics: &Metrics,
    sink: &mut dyn FnMut(&[f32]) -> Result<(), String>,
) -> Result<(), String> {
    if !super::at_least(13, 0) {
        return Err(
            "System audio requires macOS 13 or newer. Video-only sharing is still available."
                .into(),
        );
    }
    super::require_capture()?;
    let content = content::shareable()?;
    // SAFETY: immutable snapshot array.
    let main = objc2_core_graphics::CGMainDisplayID();
    let display = unsafe { content.displays() }
        .iter()
        .find(|d| unsafe { d.displayID() } == main)
        .or_else(|| unsafe { content.displays() }.iter().next())
        .ok_or("No display is available for system audio capture")?;
    let filter = super::display_filter(&display);
    let (producer, mut consumer): (HeapProd<f32>, HeapCons<f32>) = HeapRb::new(RING).split();
    let output = AudioOutput::new(AudioIvars {
        handler: Mutex::new(Handler {
            producer,
            scratch: vec![0.0; MAX_FRAMES * 2],
        }),
        packets: AtomicU64::new(0),
        overflow: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        worker: std::thread::current(),
    });
    // SAFETY: plain property setters on a fresh configuration object.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        // Video cannot be disabled before macOS 15; keep it tiny and rare.
        config.setWidth(2);
        config.setHeight(2);
        config.setMinimumFrameInterval(CMTime::new(1, 1));
        config.setQueueDepth(3);
        config.setCapturesAudio(true);
        config.setExcludesCurrentProcessAudio(true);
        config.setSampleRate(RATE as isize);
        config.setChannelCount(2);
        config
    };
    let queue = DispatchQueue::new("com.thiscord.screen.audio", None);
    // SAFETY: valid filter/configuration; `output` outlives the stream.
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*output)),
        )
    };
    for kind in [SCStreamOutputType::Audio, SCStreamOutputType::Screen] {
        // SAFETY: the output implements SCStreamOutput and outlives the stream.
        // The screen output ignores frames; it only silences SCK's log spam.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                kind,
                Some(&queue),
            )
        }
        .map_err(|e| format!("Cannot open system audio: {}", content::describe_error(&e)))?;
    }
    wait_completion(
        // SAFETY: the stream stays alive until the completion handler returns.
        |done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) },
        Duration::from_secs(10),
        "Cannot start system audio capture",
    )
    .map_err(content::permission_hint)?;
    let mut chunk = vec![0.0_f32; 4096];
    let mut previous = Instant::now();
    let mut reported = (0, 0, 0);
    let result = (|| {
        while !stop.load(Ordering::Acquire) && running() {
            if previous.elapsed() > Duration::from_secs(2) {
                return Err("System audio paused; start sharing again.".into());
            }
            previous = Instant::now();
            if output.ivars().closed.load(Ordering::Acquire) {
                return Err("System audio capture stopped; sharing stopped.".into());
            }
            let n = consumer.pop_slice(&mut chunk);
            if n > 0 {
                sink(&chunk[..n & !1])?;
                continue;
            }
            let counters = (
                output.ivars().packets.load(Ordering::Relaxed),
                output.ivars().overflow.load(Ordering::Relaxed),
                output.ivars().rejected.load(Ordering::Relaxed),
            );
            if counters != reported {
                metrics.add("audio_capture_packets", counters.0 - reported.0);
                metrics.add("audio_capture_queue_resets", counters.1 - reported.1);
                metrics.add("audio_capture_rejected", counters.2 - reported.2);
                reported = counters;
            }
            // Woken by the handler (unpark); bounded so stop is observed.
            std::thread::park_timeout(Duration::from_millis(20));
        }
        Ok(())
    })();
    let _ = wait_completion(
        // SAFETY: the stream stays alive until the completion handler returns.
        |done| unsafe { stream.stopCaptureWithCompletionHandler(Some(done)) },
        Duration::from_secs(3),
        "Cannot stop system audio capture",
    );
    drop(stream);
    drop(output);
    result
}
