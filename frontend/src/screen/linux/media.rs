//! GStreamer capture and per-layer hardware encoder pipelines.
//!
//! Capture copies each PipeWire frame into GStreamer-pooled memory of the
//! selected encoder family (VA surfaces, CUDA memory, or a system-memory copy
//! for the CPU-scaled families) at native size. PipeWire's small buffer pool is
//! therefore never held by refreshes or in-flight encodes, like the Windows
//! CopyResource into owned textures. Each layer then scales/converts to NV12 on
//! the same device and encodes; the families never download to system memory.
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use gst::prelude::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::screen::Quality;

use super::super::{
    encode::{AccessUnits, Encoded, EncoderOptions},
    worker::{LayerEncoder, Pixels},
};

/// One captured frame in encoder-family memory; both handles are refcounted.
#[derive(Clone)]
pub struct Frame {
    pub buffer: gst::Buffer,
    pub caps: gst::Caps,
}
impl Frame {
    pub fn size(&self) -> Option<(u32, u32)> {
        let s = self.caps.structure(0)?;
        let w = s.get::<i32>("width").ok()?;
        let h = s.get::<i32>("height").ok()?;
        Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// GStreamer `va` plugin (Intel/AMD, Mesa or iHD): VA surfaces end to end.
    Va,
    /// NVIDIA NVENC with CUDA scaling (needs NVRTC for `cudaconvertscale`).
    NvencCuda,
    /// Legacy gstreamer-vaapi elements.
    Vaapi,
    /// NVENC encoding with CPU scaling when CUDA kernels are unavailable.
    NvencCpuScale,
    /// No GPU encoder: clearly labelled software fallback.
    Software,
}
impl Family {
    pub fn label(self) -> &'static str {
        match self {
            Family::Va => "va",
            Family::NvencCuda => "nvenc",
            Family::Vaapi => "vaapi",
            Family::NvencCpuScale => "nvenc_cpu_scale",
            Family::Software => "software",
        }
    }
    pub fn gpu(self) -> bool {
        self != Family::Software
    }
}

/// A fully specified encoder family selected for this host.
#[derive(Debug, Clone)]
pub struct Spec {
    pub family: Family,
    /// Elements between `pipewiresrc` and the capture appsink. Empty means
    /// `pipewiresrc always-copy=true` into system memory.
    capture: &'static str,
    /// Per-layer scaler/converter element.
    scaler: &'static str,
    /// Per-layer raw caps (memory feature and format) before the encoder.
    raw: &'static str,
    pub encoder: &'static str,
    pub pixel_path: &'static str,
    /// Context types shared from the capture pipeline into encoder pipelines.
    contexts: &'static [&'static str],
}

struct Candidate {
    family: Family,
    capture: &'static str,
    capture_elements: &'static [&'static str],
    scaler: &'static str,
    raw: &'static str,
    encoders: &'static [&'static str],
    pixel_path: &'static str,
    contexts: &'static [&'static str],
}

/// Ordered by preference: zero-copy GPU paths first, software last.
fn candidates() -> [Candidate; 5] {
    [
        Candidate {
            family: Family::Va,
            capture: "vapostproc ! video/x-raw(memory:VAMemory)",
            capture_elements: &["vapostproc"],
            scaler: "vapostproc",
            raw: "video/x-raw(memory:VAMemory),format=NV12",
            // Low-power (VDENC) first on Intel; full encoder otherwise.
            encoders: &["vah264lpenc", "vah264enc"],
            pixel_path: "PipeWire DMA-BUF / VA surface copy / VA scale + NV12 / VA-API encoder",
            contexts: &["gst.va.display.handle"],
        },
        Candidate {
            family: Family::NvencCuda,
            capture: "cudaupload ! video/x-raw(memory:CUDAMemory)",
            capture_elements: &["cudaupload"],
            scaler: "cudaconvertscale",
            raw: "video/x-raw(memory:CUDAMemory),format=NV12",
            encoders: &["nvcudah264enc", "nvh264enc"],
            pixel_path: "PipeWire / CUDA upload / CUDA scale + NV12 / NVENC",
            contexts: &["gst.cuda.context"],
        },
        Candidate {
            family: Family::Vaapi,
            capture: "vaapipostproc ! video/x-raw(memory:VASurface)",
            capture_elements: &["vaapipostproc"],
            scaler: "vaapipostproc",
            raw: "video/x-raw(memory:VASurface),format=NV12",
            encoders: &["vaapih264enc"],
            pixel_path: "PipeWire / VA-API surface copy / VA-API scale + NV12 / VA-API encoder",
            contexts: &["gst.vaapi.Display"],
        },
        Candidate {
            family: Family::NvencCpuScale,
            capture: "",
            capture_elements: &[],
            scaler: "videoconvertscale",
            // NVENC converts RGB to YUV on the GPU; only scaling runs on CPU.
            raw: "video/x-raw",
            encoders: &["nvcudah264enc", "nvh264enc"],
            pixel_path: "PipeWire / CPU copy + scale / NVENC color conversion + encoder",
            contexts: &[],
        },
        Candidate {
            family: Family::Software,
            capture: "",
            capture_elements: &[],
            scaler: "videoconvertscale",
            raw: "video/x-raw,format=I420",
            encoders: &["x264enc", "openh264enc"],
            pixel_path: "PipeWire / CPU copy + scale / software encoder (no GPU encoder found)",
            contexts: &[],
        },
    ]
}

pub fn init() -> Result<(), String> {
    gst::init().map_err(|_| "Cannot initialize GStreamer for screen sharing.".to_owned())
}
fn exists(element: &str) -> bool {
    gst::ElementFactory::find(element).is_some()
}

/// Checks the required runtime plugins without opening any GPU device.
pub fn installed() -> Result<(), String> {
    init()?;
    if !exists("pipewiresrc") {
        return Err(
            "Screen sharing needs the GStreamer PipeWire plugin (gstreamer1.0-pipewire).".into(),
        );
    }
    if !exists("h264parse") || !exists("appsrc") || !exists("appsink") {
        return Err("Screen sharing needs GStreamer base and bad plugins.".into());
    }
    if !candidates()
        .iter()
        .any(|c| c.encoders.iter().any(|e| exists(e)) && exists(c.scaler))
    {
        return Err("No H.264 encoder found. Install GStreamer VA (gstreamer1.0-plugins-bad), NVIDIA NVENC or x264 plugins.".into());
    }
    Ok(())
}

/// Probes once per process: every element exists and a synthetic frame passes
/// through capture copy, scaling and encoding on this host's device.
pub fn select() -> Result<&'static Spec, String> {
    static SELECTED: OnceLock<Result<Spec, String>> = OnceLock::new();
    SELECTED
        .get_or_init(|| {
            init()?;
            installed()?;
            // Diagnostics override, e.g. THISCORD_SCREEN_ENCODER=software.
            let override_family = std::env::var("THISCORD_SCREEN_ENCODER")
                .ok()
                .filter(|family| !family.is_empty());
            for candidate in candidates() {
                if override_family
                    .as_deref()
                    .is_some_and(|f| f != candidate.family.label())
                {
                    continue;
                }
                if !exists(candidate.scaler)
                    || !candidate.capture_elements.iter().all(|e| exists(e))
                {
                    continue;
                }
                for encoder in candidate.encoders.iter().filter(|e| exists(e)) {
                    let spec = Spec {
                        family: candidate.family,
                        capture: candidate.capture,
                        scaler: candidate.scaler,
                        raw: candidate.raw,
                        encoder,
                        pixel_path: candidate.pixel_path,
                        contexts: candidate.contexts,
                    };
                    if probe(&spec) {
                        return Ok(spec);
                    }
                }
            }
            Err("No working H.264 encoder: GPU encoders failed their self-test and no software encoder is installed.".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn probe(spec: &Spec) -> bool {
    let profile = profile_for(spec.encoder, false);
    let capture = if spec.capture.is_empty() {
        String::new()
    } else {
        format!("{} ! ", spec.capture)
    };
    let description = format!(
        "videotestsrc num-buffers=3 ! video/x-raw,format=BGRx,width=640,height=360,framerate=30/1 ! \
         {capture}{scaler} ! {raw},width=320,height=180,pixel-aspect-ratio=1/1 ! \
         {encoder} name=enc ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au,profile={profile} ! fakesink",
        scaler = spec.scaler,
        raw = spec.raw,
        encoder = spec.encoder,
    );
    let Ok(pipeline) = gst::parse::launch(&description) else {
        return false;
    };
    let Some(pipeline) = pipeline.downcast_ref::<gst::Pipeline>() else {
        return false;
    };
    if let Some(enc) = pipeline.by_name("enc") {
        configure(&enc, spec.family, 1_000_000, 30, None);
    }
    let ok = pipeline.set_state(gst::State::Playing).is_ok()
        && pipeline.bus().is_some_and(|bus| {
            bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .is_some_and(|m| m.type_() == gst::MessageType::Eos)
        });
    let _ = pipeline.set_state(gst::State::Null);
    ok
}

fn template_profiles(encoder: &str) -> Vec<String> {
    let Some(factory) = gst::ElementFactory::find(encoder) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for template in factory.static_pad_templates() {
        if template.direction() != gst::PadDirection::Src {
            continue;
        }
        let caps = template.caps();
        for s in caps.iter() {
            if let Ok(list) = s.get::<gst::List>("profile") {
                profiles.extend(list.iter().filter_map(|v| v.get::<String>().ok()));
            } else if let Ok(one) = s.get::<String>("profile") {
                profiles.push(one);
            }
        }
    }
    profiles
}
/// Caps profile to request. High is used only when every viewer decodes it.
pub fn profile_for(encoder: &str, high: bool) -> &'static str {
    choose_profile(&template_profiles(encoder), high)
}
fn choose_profile(supported: &[String], high: bool) -> &'static str {
    let has = |p: &str| supported.is_empty() || supported.iter().any(|s| s == p);
    if high && has("constrained-high") {
        "constrained-high"
    } else if high && has("high") {
        "high"
    } else if has("constrained-baseline") {
        "constrained-baseline"
    } else {
        "baseline"
    }
}
fn profile_label(caps_profile: &str) -> &'static str {
    match caps_profile {
        "constrained-high" => "constrained_high",
        "high" => "high",
        _ => "baseline",
    }
}

/// Sets a property only when this element version has it; values are parsed
/// against the property's own type (enum nicks, ints, booleans).
fn set(element: &gst::Element, name: &str, value: &str) -> bool {
    let Some(pspec) = element.find_property(name) else {
        return false;
    };
    if !pspec.flags().contains(gst::glib::ParamFlags::WRITABLE) {
        return false;
    }
    match gst::glib::Value::deserialize(value, pspec.value_type()) {
        Ok(v) => {
            element.set_property_from_value(name, &v);
            true
        }
        Err(_) => false,
    }
}

/// Encoder rate control: CBR at the layer bitrate, no B-frames, ten-second
/// GOP target, ~200 ms HRD/VBV, low-latency tuning. Returns whether the
/// fastest speed preset was applied.
fn configure(
    enc: &gst::Element,
    family: Family,
    bitrate: u32,
    fps: u32,
    speed: Option<u32>,
) -> bool {
    let kbps = (bitrate / 1000).max(1).to_string();
    let gop = super::super::keyframe::gop_frames(fps).to_string();
    let vbv_kbits = (u64::from(super::super::encode::buffer_bytes(bitrate)) * 8 / 1000).to_string();
    let fast = speed == Some(0);
    match family {
        Family::Va => {
            set(enc, "rate-control", "cbr");
            set(enc, "bitrate", &kbps);
            set(enc, "cpb-size", &vbv_kbits);
            set(enc, "key-int-max", &gop);
            set(enc, "b-frames", "0");
            set(enc, "ref-frames", "1");
            fast && set(enc, "target-usage", "7")
        }
        Family::NvencCuda | Family::NvencCpuScale => {
            if !set(enc, "rate-control", "cbr") {
                set(enc, "rc-mode", "cbr");
            }
            set(enc, "bitrate", &kbps);
            set(enc, "gop-size", &gop);
            if !set(enc, "b-frames", "0") {
                set(enc, "bframes", "0");
            }
            set(enc, "vbv-buffer-size", &vbv_kbits);
            set(enc, "zerolatency", "true");
            set(enc, "zero-reorder-delay", "true");
            if !set(enc, "tune", "ultra-low-latency") {
                set(enc, "tune", "low-latency");
            }
            if fast {
                set(enc, "preset", "p1") || set(enc, "preset", "low-latency-hp")
            } else {
                if !set(enc, "preset", "p4") {
                    set(enc, "preset", "low-latency-hq");
                }
                false
            }
        }
        Family::Vaapi => {
            set(enc, "rate-control", "cbr");
            set(enc, "bitrate", &kbps);
            set(enc, "keyframe-period", &gop);
            set(enc, "max-bframes", "0");
            set(
                enc,
                "cpb-length",
                &super::super::encode::BUFFER_MS.to_string(),
            );
            fast && set(enc, "quality-level", "7")
        }
        Family::Software => {
            if enc.find_property("speed-preset").is_some() {
                // x264: CPU encoding always uses its fastest preset.
                set(enc, "pass", "cbr");
                set(enc, "bitrate", &kbps);
                set(enc, "key-int-max", &gop);
                set(enc, "bframes", "0");
                set(enc, "tune", "zerolatency");
                set(enc, "speed-preset", "ultrafast");
                set(
                    enc,
                    "vbv-buf-capacity",
                    &super::super::encode::BUFFER_MS.to_string(),
                );
                true
            } else {
                // openh264enc takes bits per second.
                set(enc, "bitrate", &bitrate.to_string());
                set(enc, "rate-control", "bitrate");
                set(enc, "gop-size", &gop);
                set(enc, "usage-type", "screen");
                set(enc, "complexity", "low");
                true
            }
        }
    }
}

type Failure = Arc<Mutex<Option<String>>>;
fn bus_failure(pipeline: &gst::Pipeline) -> Option<String> {
    let bus = pipeline.bus()?;
    let message = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])?;
    Some(match message.view() {
        gst::MessageView::Error(error) => format!(
            "{}: {}",
            error
                .src()
                .map(|s| s.name().to_string())
                .unwrap_or_default(),
            error.error()
        ),
        _ => "stream ended".into(),
    })
}

/// `pipewiresrc` (portal remote) → family copy → appsink callback.
pub struct Capture {
    pipeline: gst::Pipeline,
    failure: Failure,
}
impl Capture {
    pub fn start(
        spec: &Spec,
        fd: std::os::fd::RawFd,
        node: u32,
        mut on_frame: impl FnMut(Frame) + Send + 'static,
    ) -> Result<Self, String> {
        let copy = spec.capture.is_empty();
        let description = format!(
            "pipewiresrc name=src ! {}appsink name=sink",
            if copy {
                String::new()
            } else {
                format!("{} ! ", spec.capture)
            }
        );
        let pipeline = gst::parse::launch(&description)
            .ok()
            .and_then(|p| p.downcast::<gst::Pipeline>().ok())
            .ok_or("Cannot build the PipeWire capture pipeline.")?;
        let src = pipeline
            .by_name("src")
            .ok_or("Cannot build the PipeWire capture pipeline.")?;
        src.set_property("fd", fd);
        if !set(&src, "target-object", &node.to_string()) {
            set(&src, "path", &node.to_string());
        }
        set(&src, "always-copy", if copy { "true" } else { "false" });
        // Static sources are refreshed by the encoder worker, not resent here.
        set(&src, "keepalive-time", "0");
        let sink = pipeline
            .by_name("sink")
            .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
            .ok_or("Cannot build the PipeWire capture pipeline.")?;
        sink.set_sync(false);
        sink.set_max_buffers(1);
        sink.set_drop(true);
        if copy {
            // Force SHM/memfd negotiation; GPU families import DMA-BUF instead.
            sink.set_caps(Some(&gst::Caps::builder("video/x-raw").build()));
        }
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let (Some(buffer), Some(caps)) = (sample.buffer_owned(), sample.caps_owned())
                    {
                        on_frame(Frame { buffer, caps });
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "Cannot start PipeWire screen capture.")?;
        Ok(Self {
            pipeline,
            failure: Arc::default(),
        })
    }
    /// The PipeWire stream ended or failed (portal closed, source removed).
    pub fn failed(&self) -> bool {
        let mut failure = self.failure.lock().unwrap();
        if failure.is_none() {
            *failure = bus_failure(&self.pipeline);
        }
        failure.is_some()
    }
    pub fn failure(&self) -> Option<String> {
        self.failed();
        self.failure.lock().unwrap().clone()
    }
    /// Device contexts (VA display, CUDA context) negotiated by the capture
    /// converter, so encoder pipelines import its surfaces on the same device.
    pub fn contexts(&self, spec: &Spec) -> Vec<gst::Context> {
        pipeline_contexts(&self.pipeline, spec)
    }
}
fn pipeline_contexts(pipeline: &gst::Pipeline, spec: &Spec) -> Vec<gst::Context> {
    let mut found = Vec::new();
    for element in pipeline.iterate_elements().into_iter().flatten() {
        for kind in spec.contexts {
            if let Some(context) = element.context(kind)
                && !found
                    .iter()
                    .any(|c: &gst::Context| c.context_type() == context.context_type())
            {
                found.push(context);
            }
        }
    }
    found
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

type Outputs = Arc<Mutex<VecDeque<(u64, Vec<u8>)>>>;
/// Maximum submitted inputs without output, as on Windows.
const MAX_PENDING: usize = 4;
const STALL: Duration = Duration::from_millis(500);

/// `appsrc ! scale/NV12 ! H.264 encoder ! h264parse ! appsink` for one layer.
pub struct Layer {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    caps: gst::Caps,
    outputs: Outputs,
    pending: VecDeque<(u64, Instant)>,
    units: AccessUnits,
    force: bool,
    fps: u32,
    spec: Spec,
    profile: &'static str,
    fast: bool,
}
impl Layer {
    pub fn new(
        spec: &Spec,
        caps: &gst::Caps,
        (w, h): (u32, u32),
        quality: Quality,
        options: EncoderOptions,
        contexts: &[gst::Context],
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let requested = profile_for(spec.encoder, options.high_profile);
        let description = format!(
            "appsrc name=src ! {scaler} name=scale ! {raw},width={w},height={h},pixel-aspect-ratio=1/1 ! \
             {encoder} name=enc ! h264parse config-interval=-1 ! \
             video/x-h264,stream-format=byte-stream,alignment=au,profile={requested} ! appsink name=sink",
            scaler = spec.scaler,
            raw = spec.raw,
            encoder = spec.encoder,
        );
        let fail = |what: &str| format!("GPU H.264 encoder unavailable ({}): {what}", spec.encoder);
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| fail(&e.to_string()))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| fail("not a pipeline"))?;
        for context in contexts {
            pipeline.set_context(context);
        }
        if let Some(scale) = pipeline.by_name("scale") {
            // CPU-scaled families: spread scaling across cores.
            set(&scale, "n-threads", "4");
            set(&scale, "add-borders", "false");
        }
        let enc = pipeline.by_name("enc").ok_or_else(|| fail("no encoder"))?;
        let fast = configure(
            &enc,
            spec.family,
            quality.bitrate(),
            quality.fps,
            options.quality_speed,
        );
        let src = pipeline
            .by_name("src")
            .and_then(|s| s.downcast::<gst_app::AppSrc>().ok())
            .ok_or_else(|| fail("no appsrc"))?;
        src.set_is_live(true);
        src.set_format(gst::Format::Time);
        src.set_do_timestamp(false);
        src.set_block(false);
        let caps = with_rate(caps, quality.fps);
        src.set_caps(Some(&caps));
        let sink = pipeline
            .by_name("sink")
            .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| fail("no appsink"))?;
        sink.set_sync(false);
        sink.set_drop(false);
        let outputs: Outputs = Arc::default();
        let queue = outputs.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    // Encoders shift timestamps (x264/NVENC add a 1000 h
                    // min-PTS) and compensate in the segment; running time is
                    // the input timeline.
                    let pts = buffer
                        .pts()
                        .and_then(|pts| {
                            sample
                                .segment()
                                .and_then(|s| s.downcast_ref::<gst::ClockTime>())
                                .and_then(|s| s.to_running_time(pts))
                        })
                        .map_or(0, gst::ClockTime::useconds);
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    if map.len() > thiscord_shared::screen::MAX_FRAME_BYTES {
                        return Err(gst::FlowError::Error);
                    }
                    queue.lock().unwrap().push_back((pts, map.to_vec()));
                    wake();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| fail("cannot start"))?;
        Ok(Self {
            pipeline,
            src,
            caps,
            outputs,
            pending: VecDeque::new(),
            units: AccessUnits::new(options.high_profile),
            force: true,
            fps: quality.fps,
            spec: spec.clone(),
            profile: profile_label(requested),
            fast,
        })
    }
}
/// Pin the nominal frame rate for rate control; PipeWire reports 0/1 (VFR).
fn with_rate(caps: &gst::Caps, fps: u32) -> gst::Caps {
    let mut caps = caps.clone();
    if let Some(s) = caps.make_mut().structure_mut(0) {
        s.set("framerate", gst::Fraction::new(fps as i32, 1));
    }
    caps
}
impl Drop for Layer {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
impl LayerEncoder for Layer {
    type Frame = Frame;
    type Input = Frame;
    fn name(&self) -> &str {
        self.spec.encoder
    }
    fn profile(&self) -> &'static str {
        self.profile
    }
    fn pending(&self) -> usize {
        self.pending.len()
    }
    fn ready(&self) -> bool {
        self.pending.len() < MAX_PENDING
    }
    fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        if let Some(error) = bus_failure(&self.pipeline) {
            return Err(error);
        }
        if self
            .pending
            .front()
            .is_some_and(|(_, at)| at.elapsed() > STALL)
        {
            return Err(format!("{} stalled", self.spec.encoder));
        }
        let ready: Vec<_> = self.outputs.lock().unwrap().drain(..).collect();
        let mut frames = Vec::with_capacity(ready.len());
        for (timestamp, data) in ready {
            // Encoders may skip inputs; retire everything up to this output.
            let mut submitted = None;
            while let Some(&(stamp, at)) = self.pending.front() {
                if stamp > timestamp {
                    break;
                }
                self.pending.pop_front();
                if stamp == timestamp {
                    submitted = Some(at);
                }
            }
            let data = self
                .units
                .finish(data)
                .ok_or("Encoder produced an out-of-bounds H.264 access unit")?;
            frames.push(Encoded {
                data,
                timestamp,
                encoding_duration: submitted.map_or(Duration::ZERO, |at| at.elapsed()),
            });
        }
        Ok(frames)
    }
    fn convert(&mut self, pixels: &Pixels<Frame>) -> Result<Option<Frame>, String> {
        // A changed source format renegotiates in place; size changes rebuild.
        let caps = with_rate(&pixels.frame.caps, self.fps);
        if caps != self.caps {
            self.src.set_caps(Some(&caps));
            self.caps = caps;
        }
        Ok(Some(pixels.frame.clone()))
    }
    fn force_keyframe(&mut self) -> Result<(), String> {
        self.force = true;
        Ok(())
    }
    fn encode(&mut self, input: &Frame, micros: u64) -> Result<Vec<Encoded>, String> {
        if std::mem::take(&mut self.force) {
            // Serialized with the next buffer; h264parse repeats SPS/PPS.
            self.src.send_event(
                gst_video::DownstreamForceKeyUnitEvent::builder()
                    .all_headers(true)
                    .build(),
            );
        }
        let mut buffer = input.buffer.copy();
        {
            let buffer = buffer.get_mut().ok_or("Cannot stamp screen frame")?;
            buffer.set_pts(gst::ClockTime::from_useconds(micros));
            buffer.set_dts(gst::ClockTime::NONE);
            buffer.set_duration(gst::ClockTime::from_useconds(
                1_000_000 / u64::from(self.fps.max(1)),
            ));
        }
        self.src
            .push_buffer(buffer)
            .map_err(|e| format!("{} rejected input: {e:?}", self.spec.encoder))?;
        self.pending.push_back((micros, Instant::now()));
        self.poll()
    }
    fn quality_vs_speed(&self) -> Option<u32> {
        self.fast.then_some(0)
    }
    fn set_quality_vs_speed(&mut self, _: u32) -> Result<(), String> {
        // Presets are read at session start; the worker rebuilds once.
        Err("speed preset requires a new encoder session".into())
    }
    fn describe(&self) -> String {
        format!(
            "{} -> {} ({}, {})",
            self.spec.pixel_path,
            self.spec.encoder,
            if self.spec.family.gpu() {
                "GPU"
            } else {
                "software"
            },
            self.profile,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_prefer_zero_copy_gpu_and_keep_software_last() {
        let order: Vec<_> = candidates().iter().map(|c| c.family).collect();
        assert_eq!(
            order,
            [
                Family::Va,
                Family::NvencCuda,
                Family::Vaapi,
                Family::NvencCpuScale,
                Family::Software
            ]
        );
        for candidate in candidates() {
            assert_eq!(candidate.family.gpu(), candidate.family != Family::Software);
            assert!(!candidate.encoders.is_empty());
            // Only CPU-scaled families copy PipeWire frames to system memory.
            assert_eq!(
                candidate.capture.is_empty(),
                matches!(candidate.family, Family::NvencCpuScale | Family::Software)
            );
        }
    }

    #[test]
    fn profile_choice_follows_template_and_viewer_capability() {
        let all: Vec<String> = ["constrained-baseline", "baseline", "main", "high"]
            .map(String::from)
            .into();
        assert_eq!(choose_profile(&all, false), "constrained-baseline");
        assert_eq!(choose_profile(&all, true), "high");
        let constrained: Vec<String> = ["constrained-high", "high", "constrained-baseline"]
            .map(String::from)
            .into();
        assert_eq!(choose_profile(&constrained, true), "constrained-high");
        let main_only: Vec<String> = ["baseline", "main"].map(String::from).into();
        assert_eq!(choose_profile(&main_only, true), "baseline");
        assert_eq!(choose_profile(&[], true), "constrained-high");
        assert_eq!(profile_label("constrained-baseline"), "baseline");
    }

    /// Runs the production per-layer pipeline on synthetic frames delivered
    /// through the same appsrc path as PipeWire captures, forcing an IDR
    /// mid-stream, and decodes the output with OpenH264.
    #[test]
    fn layer_pipeline_encodes_forced_keyframes_that_decode() {
        if init().is_err() || installed().is_err() {
            eprintln!("GStreamer encoders unavailable; skipping");
            return;
        }
        let spec = select().expect("an encoder family must pass its self-test");
        eprintln!(
            "encoder family under test: {} ({})",
            spec.family.label(),
            spec.encoder
        );
        let capture = format!(
            "videotestsrc num-buffers=12 pattern=ball ! video/x-raw,format=BGRx,width=640,height=360,framerate=30/1 ! {}appsink name=sink sync=false",
            if spec.capture.is_empty() {
                String::new()
            } else {
                format!("{} ! ", spec.capture)
            }
        );
        let source = gst::parse::launch(&capture)
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let sink = source
            .by_name("sink")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        source.set_state(gst::State::Playing).unwrap();
        let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
        let wake_tx = Mutex::new(wake_tx);
        let mut layer: Option<Layer> = None;
        let mut out = Vec::new();
        let mut micros = 0;
        for index in 0..12 {
            let Ok(sample) = sink.pull_sample() else {
                break;
            };
            let frame = Frame {
                buffer: sample.buffer_owned().unwrap(),
                caps: sample.caps_owned().unwrap(),
            };
            assert_eq!(frame.size(), Some((640, 360)));
            let layer = layer.get_or_insert_with(|| {
                let contexts = pipeline_contexts(&source, spec);
                let wake = wake_tx.lock().unwrap().clone();
                Layer::new(
                    spec,
                    &frame.caps,
                    (320, 180),
                    Quality {
                        height: 720,
                        fps: 30,
                    },
                    EncoderOptions::default(),
                    &contexts,
                    Box::new(move || {
                        let _ = wake.send(());
                    }),
                )
                .unwrap()
            });
            if index == 6 {
                layer.force_keyframe().unwrap();
            }
            let pixels = Pixels {
                width: 640,
                height: 360,
                arrived: Instant::now(),
                frame,
            };
            let input = layer.convert(&pixels).unwrap().unwrap();
            while !layer.ready() {
                let _ = wake_rx.recv_timeout(Duration::from_millis(50));
                out.extend(layer.poll().unwrap());
            }
            out.extend(layer.encode(&input, micros).unwrap());
            micros += 33_333;
        }
        let layer = layer.as_mut().expect("frames were captured");
        let deadline = Instant::now() + Duration::from_secs(5);
        while out.len() < 12 && Instant::now() < deadline {
            let _ = wake_rx.recv_timeout(Duration::from_millis(50));
            out.extend(layer.poll().unwrap());
        }
        let _ = source.set_state(gst::State::Null);
        assert!(out.len() >= 10, "only {} access units", out.len());
        let keyframes = out
            .iter()
            .filter(|f| thiscord_shared::h264::recovery_frame(&f.data))
            .count();
        assert!(keyframes >= 2, "initial and forced IDRs, got {keyframes}");
        assert!(out.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
        for frame in &out {
            assert!(thiscord_shared::h264::bounded_parameter_sets(&frame.data));
        }
        // Decode with FFmpeg (the WebView decoder family) through GStreamer,
        // falling back to OpenH264; every access unit must decode.
        let decoder = ["avdec_h264", "openh264dec"]
            .into_iter()
            .find(|d| exists(d))
            .expect("an H.264 decoder for the test");
        let check = gst::parse::launch(&format!(
            "appsrc name=src format=time caps=video/x-h264,stream-format=byte-stream,alignment=au ! h264parse ! {decoder} ! appsink name=sink sync=false"
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let src = check
            .by_name("src")
            .unwrap()
            .downcast::<gst_app::AppSrc>()
            .unwrap();
        let sink = check
            .by_name("sink")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        check.set_state(gst::State::Playing).unwrap();
        for frame in &out {
            let mut buffer = gst::Buffer::from_slice(frame.data.clone());
            buffer
                .get_mut()
                .unwrap()
                .set_pts(gst::ClockTime::from_useconds(frame.timestamp));
            src.push_buffer(buffer).unwrap();
        }
        src.end_of_stream().unwrap();
        let mut decoded = 0;
        while let Ok(sample) = sink.pull_sample() {
            let caps = sample.caps().unwrap().structure(0).unwrap();
            assert_eq!(caps.get::<i32>("width").unwrap(), 320);
            assert_eq!(caps.get::<i32>("height").unwrap(), 180);
            decoded += 1;
        }
        assert!(bus_failure(&check).is_none_or(|e| e == "stream ended"));
        let _ = check.set_state(gst::State::Null);
        assert_eq!(
            decoded,
            out.len(),
            "{decoder} decoded {decoded}/{}",
            out.len()
        );
    }
}
