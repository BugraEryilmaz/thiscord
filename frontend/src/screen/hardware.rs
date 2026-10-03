//! Hardware-only Media Foundation H.264 encoder. COM/MFT work stays on the
//! dedicated encoder thread. System-memory NV12 is supported by hardware MFTs;
//! capture readback/color conversion remain CPU work, not a zero-copy claim.
use openh264::formats::{YUVBuffer, YUVSource};
use std::{
    collections::VecDeque,
    marker::PhantomData,
    mem::ManuallyDrop,
    ptr,
    rc::Rc,
    time::{Duration, Instant},
};
use thiscord_shared::screen::{MAX_FRAME_BYTES, Quality};
use windows::{
    Win32::{
        Media::MediaFoundation::*,
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Variant::VARIANT,
        },
    },
    core::{Interface, Result},
};

struct Runtime(PhantomData<Rc<()>>);
impl Runtime {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error);
            }
        }
        Ok(Self(PhantomData))
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
pub struct HardwareEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    input: u32,
    output: u32,
    requests: u32,
    pending: VecDeque<(u64, Instant)>,
    width: usize,
    height: usize,
    fps: u32,
    pub name: String,
    sps: Vec<u8>,
    pps: Vec<u8>,
    _runtime: Runtime,
}
pub struct Encoded {
    pub data: Vec<u8>,
    pub timestamp: u64,
}
fn fail() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
}
impl HardwareEncoder {
    pub fn new(width: usize, height: usize, quality: Quality) -> Result<Self> {
        if !quality.valid()
            || width < 2
            || height < 2
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || width > thiscord_shared::screen::MAX_WIDTH as usize
            || height > thiscord_shared::screen::MAX_HEIGHT as usize
        {
            return Err(fail());
        }
        let runtime = Runtime::new()?;
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let mut pointer: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut pointer,
                &mut count,
            )?;
        }
        // Take ownership of every COM reference before releasing the array.
        let activations = if pointer.is_null() {
            Vec::new()
        } else {
            let values = unsafe { std::slice::from_raw_parts_mut(pointer, count as usize) }
                .iter_mut()
                .filter_map(Option::take)
                .collect::<Vec<_>>();
            unsafe {
                CoTaskMemFree(Some(pointer.cast()));
            }
            values
        };
        for activation in activations {
            let transform: Result<IMFTransform> = unsafe { activation.ActivateObject() };
            if let Ok(transform) = transform {
                match Self::configure(&transform, width, height, quality) {
                    Ok((events, codec, input, output)) => {
                        let name = unsafe {
                            let mut text = [0u16; 256];
                            activation
                                .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut text, None)
                                .ok()
                                .map(|_| {
                                    String::from_utf16_lossy(
                                        &text[..text
                                            .iter()
                                            .position(|&c| c == 0)
                                            .unwrap_or(text.len())],
                                    )
                                })
                        }
                        .unwrap_or_else(|| "Media Foundation H.264".into());
                        return Ok(Self {
                            transform,
                            events,
                            codec,
                            input,
                            output,
                            requests: 0,
                            pending: VecDeque::new(),
                            width,
                            height,
                            fps: quality.fps,
                            name,
                            sps: Vec::new(),
                            pps: Vec::new(),
                            _runtime: runtime,
                        });
                    }
                    Err(_) => unsafe {
                        let _ = activation.ShutdownObject();
                    },
                }
            }
        }
        Err(fail())
    }
    fn configure(
        transform: &IMFTransform,
        width: usize,
        height: usize,
        quality: Quality,
    ) -> Result<(IMFMediaEventGenerator, ICodecAPI, u32, u32)> {
        unsafe {
            let attributes = transform.GetAttributes()?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            let events = transform.cast::<IMFMediaEventGenerator>()?;
            let codec = transform.cast::<ICodecAPI>()?;
            // Baseline + low latency prevents frame reordering on the RTP path.
            codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true))?;
            let _ = codec.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
            let _ = codec.SetValue(
                &CODECAPI_AVEncCommonRateControlMode,
                &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
            );
            let _ = codec.SetValue(
                &CODECAPI_AVEncCommonMeanBitRate,
                &VARIANT::from(quality.bitrate()),
            );
            let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(quality.fps));
            let (mut input, mut output) = ([0], [0]);
            if transform.GetStreamIDs(&mut input, &mut output).is_err() {
                input[0] = 0;
                output[0] = 0;
            }
            let make_type = |subtype| -> Result<IMFMediaType> {
                let media = MFCreateMediaType()?;
                media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                media.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
                media.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(quality.fps) << 32) | 1)?;
                media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
                media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                // OpenH264's RGBA conversion produces limited-range BT.601.
                media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32)?;
                media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
                Ok(media)
            };
            let media = make_type(&MFVideoFormat_H264)?;
            media.SetUINT32(&MF_MT_AVG_BITRATE, quality.bitrate())?;
            media.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
            transform.SetOutputType(output[0], &media, 0)?;
            transform.SetInputType(input[0], &make_type(&MFVideoFormat_NV12)?, 0)?;
            codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok((events, codec, input[0], output[0]))
        }
    }
    pub fn force_keyframe(&self) -> Result<()> {
        unsafe {
            self.codec
                .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))
        }
    }
    /// Nonblocking event drain with bounded output. No MFT GetEvent may block
    /// cancellation indefinitely. NeedInput credits persist between calls.
    pub fn poll(&mut self) -> Result<Vec<Encoded>> {
        if self
            .pending
            .front()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_millis(500))
        {
            return Err(fail());
        }
        let mut output = Vec::new();
        for _ in 0..32 {
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(e) => return Err(e),
            };
            unsafe {
                event.GetStatus()?.ok()?;
            }
            match unsafe { event.GetType()? } {
                kind if kind == METransformNeedInput.0 as u32 => {
                    self.requests = self.requests.saturating_add(1).min(16);
                }
                kind if kind == METransformHaveOutput.0 as u32 => {
                    if let Some(frame) = self.output()? {
                        output.push(frame);
                    }
                }
                _ => {}
            }
        }
        Ok(output)
    }
    pub fn encode(&mut self, yuv: &YUVBuffer, timestamp: u64) -> Result<Vec<Encoded>> {
        if yuv.dimensions() != (self.width, self.height) {
            return Err(fail());
        }
        let mut out = self.poll()?;
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.requests == 0 || self.pending.len() >= 4 {
            if Instant::now() >= deadline {
                return Err(fail());
            }
            std::thread::sleep(Duration::from_millis(1));
            out.extend(self.poll()?);
        }
        unsafe {
            let length = self.width * self.height * 3 / 2;
            let buffer = MFCreateMemoryBuffer(length as u32)?;
            let mut data = ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let dest = std::slice::from_raw_parts_mut(data, length);
            let (ys, us, vs) = yuv.strides();
            for row in 0..self.height {
                dest[row * self.width..(row + 1) * self.width]
                    .copy_from_slice(&yuv.y()[row * ys..row * ys + self.width]);
            }
            for row in 0..self.height / 2 {
                for column in 0..self.width / 2 {
                    let index = self.width * self.height + row * self.width + column * 2;
                    dest[index] = yuv.u()[row * us + column];
                    dest[index + 1] = yuv.v()[row * vs + column];
                }
            }
            buffer.Unlock()?;
            buffer.SetCurrentLength(length as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((timestamp * 10) as i64)?;
            sample.SetSampleDuration(10_000_000 / i64::from(self.fps))?;
            self.transform.ProcessInput(self.input, &sample, 0)?;
            self.pending.push_back((timestamp, Instant::now()));
            self.requests -= 1;
        }
        out.extend(self.poll()?);
        Ok(out)
    }
    fn output(&mut self) -> Result<Option<Encoded>> {
        unsafe {
            let info = self.transform.GetOutputStreamInfo(self.output)?;
            let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                if info.cbSize as usize > MAX_FRAME_BYTES {
                    return Err(fail());
                }
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1))?)?;
                Some(sample)
            } else {
                None
            };
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let result = self.transform.ProcessOutput(0, &mut output, &mut 0);
            let sample = ManuallyDrop::take(&mut output[0].pSample);
            drop(ManuallyDrop::take(&mut output[0].pEvents));
            if let Err(error) = result {
                if error.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    let media = self.transform.GetOutputAvailableType(self.output, 0)?;
                    self.transform.SetOutputType(self.output, &media, 0)?;
                    return Ok(None);
                }
                return Err(error);
            }
            let sample = sample.ok_or_else(fail)?;
            let time = sample.GetSampleTime()?;
            if let Some(index) = self
                .pending
                .iter()
                .position(|(timestamp, _)| *timestamp == (time as u64 / 10))
            {
                self.pending.remove(index);
            } else {
                return Err(fail());
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let length = buffer.GetCurrentLength()? as usize;
            if length > MAX_FRAME_BYTES || time < 0 {
                return Err(fail());
            }
            if length == 0 {
                return Ok(None);
            }
            let mut data = ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let mut bytes = std::slice::from_raw_parts(data, length).to_vec();
            buffer.Unlock()?;
            let mut idr = false;
            for nal in openh264::nal_units(&bytes) {
                let nal = nal.strip_prefix(&[0, 0, 1]).unwrap_or(nal);
                match nal.first().map(|b| b & 31) {
                    Some(7) => self.sps = nal.to_vec(),
                    Some(8) => self.pps = nal.to_vec(),
                    Some(5) => idr = true,
                    _ => {}
                }
            }
            // Every recovery point must include parameter sets for late joiners.
            if idr && !self.sps.is_empty() && !self.pps.is_empty() {
                let mut prefix =
                    Vec::with_capacity(bytes.len() + self.sps.len() + self.pps.len() + 8);
                prefix.extend_from_slice(&[0, 0, 0, 1]);
                prefix.extend_from_slice(&self.sps);
                prefix.extend_from_slice(&[0, 0, 0, 1]);
                prefix.extend_from_slice(&self.pps);
                prefix.append(&mut bytes);
                bytes = prefix;
            }
            if bytes.len() > MAX_FRAME_BYTES || !super::bounded_parameter_sets(&bytes) {
                return Err(fail());
            }
            Ok(Some(Encoded {
                data: bytes,
                timestamp: time as u64 / 10,
            }))
        }
    }
}
impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}
