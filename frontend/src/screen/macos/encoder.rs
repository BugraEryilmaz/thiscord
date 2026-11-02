//! VideoToolbox hardware H.264 for one layer. Input is the captured
//! IOSurface; when its size differs from the session, VideoToolbox's own GPU
//! pixel transfer scales it. Output is converted from AVCC to Annex B with
//! SPS/PPS on every IDR and validated against the negotiated profile.
use super::Frame;
use crate::screen::{
    encode::{AccessUnits, Encoded, EncoderOptions, avcc_to_annex_b, buffer_bytes},
    keyframe::{RECOVERY_INTERVAL, gop_frames},
    worker::{LayerEncoder, Pixels},
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMVideoCodecType_H264,
};
use objc2_video_toolbox::*;
use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use thiscord_shared::screen::{MAX_FRAME_BYTES, Quality};

/// Matches the Windows MFT admission limit.
const MAX_PENDING: usize = 4;
/// Reference frames requested from VideoToolbox. Its automatic level otherwise
/// advertises the level's whole DPB (12 frames at 1440p60, 5 at 4K), which
/// receivers reject: their bound is four 4K frames of reference memory. Four
/// frames fit that bound at every size up to 4K.
const MAX_REFERENCES: i32 = 4;
/// H.264 level 5.0 limits (macroblocks per frame / per second). Any stream
/// within them advertises at most 110400 DPB macroblocks, inside the receiver
/// reference budget, so this is the fallback when the reference cap is missing.
const LEVEL_5_0_FRAME_MBS: u64 = 22_080;
const LEVEL_5_0_MBS_PER_SECOND: u64 = 589_824;
const STALL: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Output {
    frames: VecDeque<Encoded>,
    /// Submitted (presentation micros, submit time), in submission order.
    pending: VecDeque<(u64, Instant)>,
    error: Option<String>,
    dropped: u64,
}
/// Shared with the VideoToolbox output callback (its refcon).
struct Callback {
    output: Mutex<Output>,
    units: Mutex<AccessUnits>,
    wake: Box<dyn Fn() + Send + Sync>,
}

pub struct VideoToolbox {
    session: CFRetained<VTCompressionSession>,
    callback: *const Callback,
    name: String,
    profile: &'static str,
    speed: bool,
    force_next: bool,
    last_pts: Option<u64>,
    fps: u32,
    pub buffer_applied: bool,
    pub gop_applied: bool,
    pub low_latency: bool,
    /// Whether VideoToolbox accepted `MAX_REFERENCES` (macOS 13+).
    pub references_capped: bool,
    /// Session size; smaller than requested only without the reference cap.
    pub size: (u32, u32),
}
// SAFETY: a VTCompressionSession may be driven from any one thread at a time;
// the worker owns it exclusively. `callback` is an Arc kept alive until after
// the session is invalidated in Drop, and its contents are Sync.
unsafe impl Send for VideoToolbox {}

fn cf_bool(value: bool) -> &'static CFType {
    CFBoolean::new(value)
}
fn set(session: &VTCompressionSession, key: &CFString, value: &CFType) -> bool {
    // SAFETY: valid session, constant key and a CF value of the documented type.
    unsafe { VTSessionSetProperty(session, key, Some(value)) == 0 }
}

impl VideoToolbox {
    pub fn new(
        (width, height): (u32, u32),
        quality: Quality,
        options: EncoderOptions,
        wake: Box<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let callback = Arc::new(Callback {
            output: Mutex::default(),
            units: Mutex::new(AccessUnits::new(false)),
            wake,
        });
        // Low-latency rate control (FaceTime's mode) first; it may reject a
        // baseline profile, in which case the ordinary realtime mode is used.
        let mut last_error = String::new();
        for low_latency in [true, false] {
            let create = |(width, height)| {
                Self::create(width, height, quality, options, low_latency, &callback)
            };
            match create((width, height)) {
                Ok(encoder) if encoder.references_capped => return Ok(encoder),
                Ok(encoder) => {
                    // No reference cap (macOS 12): stay within level 5.0;
                    // VideoToolbox scales the larger capture on the GPU.
                    let fitted = level_5_0_size((width, height), quality.fps);
                    if fitted == (width, height) {
                        return Ok(encoder);
                    }
                    drop(encoder);
                    match create(fitted) {
                        Ok(encoder) => return Ok(encoder),
                        Err(error) => last_error = error,
                    }
                }
                Err(error) => last_error = error,
            }
        }
        Err(format!(
            "Hardware H.264 encoding is unavailable ({last_error})"
        ))
    }

    fn create(
        width: u32,
        height: u32,
        quality: Quality,
        options: EncoderOptions,
        low_latency: bool,
        callback: &Arc<Callback>,
    ) -> Result<Self, String> {
        // SAFETY: constant CFString keys exported by VideoToolbox.
        let (require, low) = unsafe {
            (
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
            )
        };
        let spec = if low_latency {
            CFDictionary::<CFString, CFType>::from_slices(
                &[require, low],
                &[cf_bool(true), cf_bool(true)],
            )
        } else {
            CFDictionary::<CFString, CFType>::from_slices(&[require], &[cf_bool(true)])
        };
        let refcon = Arc::into_raw(callback.clone());
        let mut raw: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: valid dimensions/dictionary; `refcon` stays alive until the
        // session is invalidated (Drop, or below on failure).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                None,
                None,
                Some(output_callback),
                refcon.cast_mut().cast(),
                NonNull::from(&mut raw),
            )
        };
        let Some(raw) = NonNull::new(raw).filter(|_| status == 0) else {
            // SAFETY: reclaim the reference handed to the failed session.
            drop(unsafe { Arc::from_raw(refcon) });
            return Err(format!("VTCompressionSessionCreate {status}"));
        };
        // SAFETY: +1 reference returned by the Create function.
        let session = unsafe { CFRetained::from_raw(raw) };
        let mut encoder = Self {
            session,
            callback: refcon,
            name: if low_latency {
                "VideoToolbox H.264 (hardware, low-latency)".into()
            } else {
                "VideoToolbox H.264 (hardware)".into()
            },
            profile: "baseline",
            speed: false,
            force_next: true,
            last_pts: None,
            fps: quality.fps,
            buffer_applied: false,
            gop_applied: false,
            low_latency,
            references_capped: false,
            size: (width, height),
        };
        encoder.configure(quality, options)?;
        // SAFETY: valid session; allocates encoder resources up front.
        unsafe { encoder.session.prepare_to_encode_frames() };
        Ok(encoder)
    }

    fn configure(&mut self, quality: Quality, options: EncoderOptions) -> Result<(), String> {
        let session = &*self.session;
        // SAFETY: all keys/values below are constant VideoToolbox CFStrings.
        unsafe {
            set(session, kVTCompressionPropertyKey_RealTime, cf_bool(true));
            set(
                session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                cf_bool(false),
            );
            // Constrained High only when every viewer decodes High; plain
            // High (no B-frames) is accepted by receivers as a fallback.
            let profiles: &[(&CFString, &'static str)] = if options.high_profile {
                &[
                    (
                        kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
                        "constrained_high",
                    ),
                    (kVTProfileLevel_H264_High_AutoLevel, "high"),
                ]
            } else {
                &[
                    (
                        kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel,
                        "baseline",
                    ),
                    (kVTProfileLevel_H264_Baseline_AutoLevel, "baseline"),
                ]
            };
            self.profile = profiles
                .iter()
                .find(|(level, _)| set(session, kVTCompressionPropertyKey_ProfileLevel, level))
                .map(|(_, name)| *name)
                .ok_or("profile rejected")?;
            // SAFETY: constant key; macOS 13+ (weakly linked, checked first).
            self.references_capped = super::at_least(13, 0)
                && set(
                    session,
                    kVTCompressionPropertyKey_ReferenceBufferCount,
                    &CFNumber::new_i32(MAX_REFERENCES),
                );
            let high = self.profile != "baseline";
            *(*self.callback).units.lock().unwrap() = AccessUnits::new(high);
            if high {
                set(
                    session,
                    kVTCompressionPropertyKey_H264EntropyMode,
                    kVTH264EntropyMode_CABAC,
                );
            }
            if !set(
                session,
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i64(i64::from(quality.bitrate())),
            ) {
                return Err("bitrate rejected".into());
            }
            // The H.264 HRD equivalent: 200 ms of codec output, in bytes.
            let limits = CFArray::<CFNumber>::from_objects(&[
                &CFNumber::new_i64(i64::from(buffer_bytes(quality.bitrate()))),
                &CFNumber::new_f64(f64::from(crate::screen::encode::BUFFER_MS) / 1000.0),
            ]);
            self.buffer_applied = set(
                session,
                kVTCompressionPropertyKey_DataRateLimits,
                limits.as_opaque(),
            );
            set(
                session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_i32(quality.fps as i32),
            );
            self.gop_applied = set(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i32(gop_frames(quality.fps) as i32),
            ) & set(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                &CFNumber::new_f64(RECOVERY_INTERVAL.as_secs_f64()),
            );
            set(
                session,
                kVTCompressionPropertyKey_ColorPrimaries,
                objc2_core_video::kCVImageBufferColorPrimaries_ITU_R_709_2,
            );
            set(
                session,
                kVTCompressionPropertyKey_TransferFunction,
                objc2_core_video::kCVImageBufferTransferFunction_ITU_R_709_2,
            );
            set(
                session,
                kVTCompressionPropertyKey_YCbCrMatrix,
                objc2_core_video::kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            );
        }
        if options.quality_speed == Some(0) {
            self.speed = self.prioritize_speed();
        }
        Ok(())
    }
    fn prioritize_speed(&self) -> bool {
        // SAFETY: constant key; macOS 12+ (weakly linked, checked at runtime).
        super::at_least(12, 0)
            && set(
                &self.session,
                unsafe { kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality },
                cf_bool(true),
            )
    }
    fn callback(&self) -> &Callback {
        // SAFETY: the Arc is alive until Drop.
        unsafe { &*self.callback }
    }
    fn drain(&self) -> Result<Vec<Encoded>, String> {
        let mut output = self.callback().output.lock().unwrap();
        if let Some(error) = output.error.take() {
            return Err(error);
        }
        if output
            .pending
            .front()
            .is_some_and(|(_, at)| at.elapsed() > STALL)
        {
            return Err("hardware encoder stalled".into());
        }
        Ok(output.frames.drain(..).collect())
    }
}

/// Largest even size with the source aspect that stays within H.264 level 5.0
/// at `fps`; sizes already within it are returned unchanged.
pub(crate) fn level_5_0_size((width, height): (u32, u32), fps: u32) -> (u32, u32) {
    let limit = LEVEL_5_0_FRAME_MBS.min(LEVEL_5_0_MBS_PER_SECOND / u64::from(fps.max(1)));
    let fits = |(w, h): (u32, u32)| u64::from(w.div_ceil(16)) * u64::from(h.div_ceil(16)) <= limit;
    if fits((width, height)) {
        return (width, height);
    }
    let mut scale = (limit as f64 * 256.0 / (f64::from(width) * f64::from(height))).sqrt();
    loop {
        let size = (
            ((f64::from(width) * scale) as u32 & !1).max(2),
            ((f64::from(height) * scale) as u32 & !1).max(2),
        );
        if fits(size) || size == (2, 2) {
            return size;
        }
        scale *= 0.99;
    }
}

impl Drop for VideoToolbox {
    fn drop(&mut self) {
        // SAFETY: invalidation is synchronous; no callback runs afterwards,
        // so the refcon Arc can be released.
        unsafe {
            self.session.invalidate();
            drop(Arc::from_raw(self.callback));
        }
    }
}

impl LayerEncoder for VideoToolbox {
    type Frame = Frame;
    type Input = Frame;
    fn name(&self) -> &str {
        &self.name
    }
    fn profile(&self) -> &'static str {
        self.profile
    }
    fn pending(&self) -> usize {
        self.callback().output.lock().unwrap().pending.len()
    }
    fn ready(&self) -> bool {
        self.pending() < MAX_PENDING
    }
    fn poll(&mut self) -> Result<Vec<Encoded>, String> {
        self.drain()
    }
    /// The capture surface is already NV12; VideoToolbox scales it on the GPU.
    fn convert(&mut self, pixels: &Pixels<Frame>) -> Result<Option<Frame>, String> {
        Ok(Some(pixels.frame.clone()))
    }
    fn force_keyframe(&mut self) -> Result<(), String> {
        // Applied to the next input through the frame-properties dictionary.
        self.force_next = true;
        Ok(())
    }
    fn encode(&mut self, input: &Frame, micros: u64) -> Result<Vec<Encoded>, String> {
        // VideoToolbox requires strictly increasing presentation times; a held
        // refresh image can carry an older capture time than a fresh frame.
        let pts = self.last_pts.map_or(micros, |last| micros.max(last + 1));
        self.last_pts = Some(pts);
        self.callback()
            .output
            .lock()
            .unwrap()
            .pending
            .push_back((pts, Instant::now()));
        // A keyframe is forced whenever the worker requested one for this
        // input; the worker calls `force_keyframe` immediately before.
        let force = std::mem::take(&mut self.force_next);
        // SAFETY: constant key and boolean value.
        let properties = force.then(|| {
            CFDictionary::<CFString, CFType>::from_slices(
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[cf_bool(true)],
            )
        });
        let mut flags = VTEncodeInfoFlags(0);
        // SAFETY: valid session and retained image buffer; the PTS (micros)
        // round-trips through sourceFrameRefCon to identify the frame.
        let status = unsafe {
            self.session.encode_frame(
                input.buffer(),
                CMTime::new(pts as i64, 1_000_000),
                CMTime::new(1, self.fps.max(1) as i32),
                properties.as_deref().map(|p| p.as_opaque()),
                pts as usize as *mut c_void,
                &mut flags,
            )
        };
        if status != 0 {
            let mut output = self.callback().output.lock().unwrap();
            output.pending.retain(|(at, _)| *at != pts);
            return Err(format!("VTCompressionSessionEncodeFrame {status}"));
        }
        self.drain()
    }
    /// Low-latency rate control is already the speed-first realtime mode
    /// (it rejects the separate speed property), so it reports 0 as well.
    fn quality_vs_speed(&self) -> Option<u32> {
        (self.speed || self.low_latency).then_some(0)
    }
    fn set_quality_vs_speed(&mut self, value: u32) -> Result<(), String> {
        if value == 0 && self.prioritize_speed() {
            self.speed = true;
            Ok(())
        } else {
            Err("speed hint rejected".into())
        }
    }
    fn describe(&self) -> String {
        format!(
            "ScreenCaptureKit NV12 IOSurface -> {} ({}, {}x{})",
            self.name, self.profile, self.size.0, self.size.1
        )
    }
}

/// VideoToolbox output callback; runs on a VideoToolbox thread.
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    frame: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: refcon is the Callback Arc kept alive by the encoder.
    let callback = unsafe { &*refcon.cast::<Callback>() };
    let pts = frame as usize as u64;
    let result = if status != 0 {
        Some(Err(format!("hardware encode failed ({status})")))
    } else if flags.contains(VTEncodeInfoFlags::FrameDropped) {
        None
    } else {
        // SAFETY: a valid sample buffer for the duration of the callback.
        unsafe { sample.as_ref() }.map(|sample| annex_b(sample, &callback.units))
    };
    {
        let mut output = callback.output.lock().unwrap();
        let submitted = output
            .pending
            .iter()
            .position(|(at, _)| *at == pts)
            .and_then(|index| output.pending.remove(index));
        match (result, submitted) {
            (Some(Ok(data)), Some((_, at))) => output.frames.push_back(Encoded {
                data,
                timestamp: pts,
                encoding_duration: at.elapsed(),
            }),
            (Some(Err(error)), _) => output.error = Some(error),
            (Some(Ok(_)), None) => output.error = Some("unexpected encoder output".into()),
            (None, _) => output.dropped += 1,
        }
    }
    (callback.wake)();
}

/// AVCC sample -> validated Annex B access unit with parameter sets on IDRs.
fn annex_b(sample: &CMSampleBuffer, units: &Mutex<AccessUnits>) -> Result<Vec<u8>, String> {
    // SAFETY: valid sample buffer; block/format objects are retained.
    let (block, format) = unsafe { (sample.data_buffer(), sample.format_description()) };
    let block = block.ok_or("encoder produced no data")?;
    let format = format.ok_or("encoder produced no format")?;
    // SAFETY: plain length query.
    let length = unsafe { block.data_length() };
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err("encoded frame exceeds bounds".into());
    }
    let mut avcc = vec![0_u8; length];
    // SAFETY: destination has exactly `length` bytes.
    let status = unsafe { block.copy_data_bytes(0, length, NonNull::from(&mut avcc[..]).cast()) };
    if status != 0 {
        return Err(format!("CMBlockBufferCopyDataBytes {status}"));
    }
    let (sets, header) = parameter_sets(&format)?;
    let mut out = Vec::with_capacity(length + 64);
    let start = out.len();
    avcc_to_annex_b(&avcc, header, &mut out).ok_or("malformed encoder output")?;
    let idr = thiscord_shared::h264::nal_units(&out[start..])
        .any(|n| n.first().map(|b| b & 31) == Some(5));
    if idr {
        let mut prefixed = Vec::with_capacity(out.len() + 64);
        for set in sets {
            prefixed.extend_from_slice(&[0, 0, 0, 1]);
            prefixed.extend_from_slice(set);
        }
        prefixed.append(&mut out);
        out = prefixed;
    }
    units
        .lock()
        .unwrap()
        .finish(out)
        .map_err(|error| format!("encoder output violates the negotiated H.264 bounds: {error}"))
}

/// SPS/PPS and the AVCC NAL length size from the session's format description.
fn parameter_sets(format: &CMFormatDescription) -> Result<(Vec<&[u8]>, usize), String> {
    let mut count = 0;
    let mut header = 0;
    // SAFETY: valid H.264 format description; out-pointers are valid.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut count,
            &mut header,
        )
    };
    if status != 0 || count == 0 || !(1..=4).contains(&header) {
        return Err(format!("no H.264 parameter sets ({status})"));
    }
    let mut sets = Vec::with_capacity(count);
    for index in 0..count.min(8) {
        let (mut data, mut size) = (std::ptr::null(), 0);
        // SAFETY: index < count; the returned bytes live as long as `format`.
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format,
                index,
                &mut data,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 || data.is_null() || size == 0 {
            return Err(format!("cannot read H.264 parameter set ({status})"));
        }
        // SAFETY: CoreMedia returned `size` readable bytes owned by `format`.
        sets.push(unsafe { std::slice::from_raw_parts(data, size) });
    }
    Ok((sets, header as usize))
}
