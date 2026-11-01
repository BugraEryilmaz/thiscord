//! `--codecs`: evidence for codec/profile choices on this adapter. Synthetic
//! FP16/BGRA scenes only; no capture, accounts or network. NV12 readback and
//! Windows' software H.264 decoder (which, unlike OpenH264, decodes High) are
//! probe-only measurement tools and never part of the production path.
use super::scene::Scene;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use thiscord_frontend::screen::{
    gpu,
    hardware::{EncoderOptions, HardwareEncoder},
};
use thiscord_shared::screen::Quality;
use windows::{
    Win32::{
        Graphics::{Direct3D11::*, Dxgi::Common::*, Dxgi::IDXGIDevice},
        Media::MediaFoundation::*,
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
            Variant::VARIANT,
        },
    },
    core::{GUID, Interface},
};

type Error = Box<dyn std::error::Error>;

pub fn run(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<(), Error> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    inventory(device)?;
    for (width, height) in [(1920u32, 1080u32), (3840, 2160)] {
        fidelity(device, context, width, height)?;
    }
    temporal_layers(device, context)?;
    long_term_reference(device, context)?;
    Ok(())
}

/// Hardware encoder MFTs on this adapter, per codec, plus the H.264 CodecAPI
/// properties that layered/LTR recovery would need.
fn inventory(device: &ID3D11Device) -> Result<(), Error> {
    let adapter = unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()? };
    let luid = (u64::from(adapter.AdapterLuid.HighPart as u32) << 32)
        | u64::from(adapter.AdapterLuid.LowPart);
    for (codec, subtype) in [
        ("H.264", MFVideoFormat_H264),
        ("HEVC", MFVideoFormat_HEVC),
        ("AV1", MFVideoFormat_AV1),
    ] {
        let activations = unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or("attributes")?;
            attributes.SetUINT64(&MFT_ENUM_ADAPTER_LUID, luid)?;
            let (input, output) = (
                MFT_REGISTER_TYPE_INFO {
                    guidMajorType: MFMediaType_Video,
                    guidSubtype: MFVideoFormat_NV12,
                },
                MFT_REGISTER_TYPE_INFO {
                    guidMajorType: MFMediaType_Video,
                    guidSubtype: subtype,
                },
            );
            let (mut pointer, mut count) = (std::ptr::null_mut(), 0);
            MFTEnum2(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &attributes,
                &mut pointer,
                &mut count,
            )?;
            let values: Vec<IMFActivate> = if pointer.is_null() {
                Vec::new()
            } else {
                let values = std::slice::from_raw_parts_mut(pointer, count as usize)
                    .iter_mut()
                    .filter_map(Option::take)
                    .collect();
                windows::Win32::System::Com::CoTaskMemFree(Some(pointer.cast()));
                values
            };
            values
        };
        if activations.is_empty() {
            println!("Codec inventory: {codec}: no hardware encoder on this adapter");
        }
        for activation in activations {
            let name = unsafe {
                let mut text = [0u16; 256];
                activation
                    .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut text, None)
                    .map(|_| {
                        String::from_utf16_lossy(
                            &text[..text.iter().position(|&c| c == 0).unwrap_or(text.len())],
                        )
                    })
                    .unwrap_or_else(|_| "unnamed".into())
            };
            let transform: IMFTransform = unsafe { activation.ActivateObject()? };
            let mut properties = Vec::new();
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                for (label, property) in [
                    ("temporal_layers", CODECAPI_AVEncVideoTemporalLayerCount),
                    ("ltr_buffer", CODECAPI_AVEncVideoLTRBufferControl),
                    ("max_ref_frames", CODECAPI_AVEncVideoMaxNumRefFrame),
                    ("cabac", CODECAPI_AVEncH264CABACEnable),
                    ("quality_vs_speed", CODECAPI_AVEncCommonQualityVsSpeed),
                ] {
                    let supported = unsafe { api.IsSupported(&property) }.is_ok();
                    properties.push(format!("{label}={supported}"));
                }
            }
            // Accepts a 1080p60 NV12 -> codec configuration (type negotiation
            // only; no D3D manager, so no frames are encoded here).
            let configurable = unsafe {
                let make = |subtype: &GUID| -> windows::core::Result<IMFMediaType> {
                    let media = MFCreateMediaType()?;
                    media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                    media.SetUINT64(&MF_MT_FRAME_SIZE, (1920 << 32) | 1080)?;
                    media.SetUINT64(&MF_MT_FRAME_RATE, (60 << 32) | 1)?;
                    media
                        .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    Ok(media)
                };
                (|| -> windows::core::Result<()> {
                    transform
                        .GetAttributes()?
                        .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                    let output = make(&subtype)?;
                    output.SetUINT32(&MF_MT_AVG_BITRATE, 6_000_000)?;
                    transform.SetOutputType(0, &output, 0)?;
                    transform.SetInputType(0, &make(&MFVideoFormat_NV12)?, 0)
                })()
                .map_or_else(|e| format!("{:#x}", e.code().0), |_| "ok".into())
            };
            properties.push(format!("configure_1080p60={configurable}"));
            println!(
                "Codec inventory: {codec}: {name}; {}",
                properties.join(", ")
            );
            unsafe {
                let _ = activation.ShutdownObject();
            }
        }
    }
    Ok(())
}

/// Windows' H.264 decoder MFT (software, decodes Constrained Baseline and High).
struct Decoder {
    transform: IMFTransform,
    width: usize,
    height: usize,
}
impl Decoder {
    fn new(width: u32, height: u32) -> Result<Self, Error> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(width) << 32) | u64::from(height),
            )?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (60 << 32) | 1)?;
            transform.SetInputType(0, &input, 0)?;
            let decoder = Self {
                transform,
                width: width as usize,
                height: height as usize,
            };
            decoder.select_nv12()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }
    fn select_nv12(&self) -> Result<(), Error> {
        for index in 0.. {
            let media = unsafe { self.transform.GetOutputAvailableType(0, index)? };
            if unsafe { media.GetGUID(&MF_MT_SUBTYPE)? } == MFVideoFormat_NV12 {
                unsafe { self.transform.SetOutputType(0, &media, 0)? };
                return Ok(());
            }
        }
        Err("decoder has no NV12 output".into())
    }
    /// Decodes one access unit (or drains with `None`). Returns cropped luma
    /// planes with their sample times: the decoder may output with latency.
    fn decode(&mut self, input: Option<(&[u8], i64)>) -> Result<Vec<(i64, Vec<u8>)>, Error> {
        unsafe {
            if let Some((data, time)) = input {
                let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
                let mut pointer = std::ptr::null_mut();
                buffer.Lock(&mut pointer, None, None)?;
                std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
                buffer.Unlock()?;
                buffer.SetCurrentLength(data.len() as u32)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(time)?;
                sample.SetSampleDuration(166_667)?;
                self.transform.ProcessInput(0, &sample, 0)?;
            } else {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            }
        }
        let mut pictures = Vec::new();
        loop {
            let info = unsafe { self.transform.GetOutputStreamInfo(0)? };
            let sample = unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1))?)?;
                sample
            };
            let mut output = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut 0) };
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut output[0].pSample) };
            drop(unsafe { std::mem::ManuallyDrop::take(&mut output[0].pEvents) });
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Ok(pictures);
                }
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.select_nv12()?,
                Err(error) => return Err(error.into()),
                Ok(()) => {
                    let sample = sample.ok_or("no output sample")?;
                    let time = unsafe { sample.GetSampleTime()? };
                    let buffer = unsafe { sample.GetBufferByIndex(0)? };
                    let mut luma = vec![0u8; self.width * self.height];
                    if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
                        let (mut scanline, mut pitch) = (std::ptr::null_mut(), 0i32);
                        unsafe { planar.Lock2D(&mut scanline, &mut pitch)? };
                        for row in 0..self.height {
                            let line = unsafe {
                                std::slice::from_raw_parts(
                                    scanline.offset(row as isize * pitch as isize),
                                    self.width,
                                )
                            };
                            luma[row * self.width..(row + 1) * self.width].copy_from_slice(line);
                        }
                        unsafe { planar.Unlock2D()? };
                    } else {
                        let media = unsafe { self.transform.GetOutputCurrentType(0)? };
                        let stride = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
                            .map(|s| s as i32 as usize)
                            .unwrap_or(self.width);
                        let mut pointer = std::ptr::null_mut();
                        let mut length = 0;
                        unsafe { buffer.Lock(&mut pointer, None, Some(&mut length))? };
                        let bytes = unsafe { std::slice::from_raw_parts(pointer, length as usize) };
                        for row in 0..self.height {
                            luma[row * self.width..(row + 1) * self.width]
                                .copy_from_slice(&bytes[row * stride..row * stride + self.width]);
                        }
                        unsafe { buffer.Unlock()? };
                    }
                    pictures.push((time, luma));
                }
            }
        }
    }
}

fn staging(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D, Error> {
    let mut texture = None;
    unsafe {
        device.CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut texture),
        )?;
    }
    Ok(texture.ok_or("staging")?)
}
fn read_luma(
    context: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    surface: &ID3D11Texture2D,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, Error> {
    let mut luma = vec![0u8; width * height];
    unsafe {
        let _guard = gpu::ContextGuard::lock(context)?;
        context.CopyResource(staging, surface);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        for row in 0..height {
            luma[row * width..(row + 1) * width].copy_from_slice(std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                width,
            ));
        }
        context.Unmap(staging, 0);
    }
    Ok(luma)
}
fn squared_error(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).pow(2) as u64)
        .sum()
}
fn psnr(error: u64, samples: u64) -> f64 {
    10.0 * (255.0f64.powi(2) * samples as f64 / error.max(1) as f64).log10()
}
/// Synchronously encodes one surface; probe-only.
fn encode_one(
    encoder: &mut HardwareEncoder,
    surface: Arc<ID3D11Texture2D>,
    timestamp: u64,
) -> Result<Vec<u8>, Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !encoder.ready() {
        encoder.poll()?;
        if Instant::now() > deadline {
            return Err("encoder input timeout".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut encoded = encoder.encode_texture(surface, timestamp)?;
    while encoded.is_empty() {
        if Instant::now() > deadline {
            return Err("encoder output timeout".into());
        }
        std::thread::sleep(Duration::from_millis(1));
        encoded.extend(encoder.poll()?);
    }
    Ok(encoded.remove(0).data)
}
fn slices(data: &[u8]) -> impl Iterator<Item = (u8, u8)> + '_ {
    thiscord_shared::h264::nal_units(data)
        .filter_map(|nal| nal.first().map(|b| (b & 31, (b >> 5) & 3)))
}

/// Same synthetic scene, same CBR target: baseline vs High, driver-default vs
/// fastest speed hint. Equal target bitrate makes PSNR differences comparable.
fn fidelity(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    width: u32,
    height: u32,
) -> Result<(), Error> {
    let quality = Quality { height, fps: 60 };
    let scene = Scene::new(device, width, height)?;
    let processor = gpu::Processor::new(device, width, height)?;
    let staging = staging(device, width, height)?;
    let frames = 120u64;
    let mut results = Vec::new();
    for (high, speed) in [
        (false, None),
        (false, Some(0)),
        (true, None),
        (true, Some(0)),
    ] {
        let mut encoder = HardwareEncoder::new_gpu_with(
            width as usize,
            height as usize,
            quality,
            device,
            EncoderOptions {
                high_profile: high,
                quality_speed: speed,
            },
        )?;
        let mut decoder = Decoder::new(width, height)?;
        let (mut bytes, mut error, mut samples) = (0usize, 0u64, 0u64);
        let mut sps_profile = None;
        let mut service = Duration::ZERO;
        let mut references = std::collections::HashMap::new();
        let mut decoded = 0u64;
        for frame in 0..frames {
            scene.draw(context, frame)?;
            let surface = processor
                .process(&scene.texture, 3.0, true)?
                .ok_or("no NV12 surface")?;
            let reference =
                read_luma(context, &staging, &surface, width as usize, height as usize)?;
            let at = Instant::now();
            let data = encode_one(&mut encoder, surface, frame * 16_667)?;
            service += at.elapsed();
            if sps_profile.is_none() {
                sps_profile = thiscord_shared::h264::parameter_set_profile(&data, true)
                    .map_err(|e| e.to_string())?;
            }
            bytes += data.len();
            references.insert(frame as i64 * 166_667, reference);
            for (time, picture) in decoder.decode(Some((&data, frame as i64 * 166_667)))? {
                let reference = references
                    .remove(&time)
                    .ok_or("unmatched decoded picture")?;
                error += squared_error(&picture, &reference);
                samples += u64::from(width * height);
                decoded += 1;
            }
        }
        for (time, picture) in decoder.decode(None)? {
            let reference = references
                .remove(&time)
                .ok_or("unmatched drained picture")?;
            error += squared_error(&picture, &reference);
            samples += u64::from(width * height);
            decoded += 1;
        }
        if decoded != frames {
            return Err(format!("decoded {decoded}/{frames} pictures").into());
        }
        if samples == 0 {
            return Err("decoder produced no pictures".into());
        }
        let score = psnr(error, samples);
        println!(
            "Codec fidelity: {width}x{height}/60 target={:.1} Mbit/s profile={} (validated SPS {:?}) speed={speed:?} readback={:?} luma_PSNR_dB={score:.2} actual={:.2} Mbit/s sync_encode_ms={:.2}",
            quality.bitrate() as f64 / 1e6,
            encoder.profile,
            sps_profile,
            encoder.quality_vs_speed(),
            bytes as f64 * 8.0 / (frames as f64 / 60.0) / 1e6,
            service.as_secs_f64() * 1000.0 / frames as f64,
        );
        if high && sps_profile != Some(thiscord_shared::h264::Profile::High) {
            println!("Codec fidelity: driver produced no High SPS; production falls back safely");
        }
        results.push(score);
    }
    println!(
        "Codec fidelity summary {width}x{height}: High vs baseline {:+.2} dB (default hint), {:+.2} dB (fastest); default vs fastest hint {:+.2} dB (baseline)",
        results[2] - results[0],
        results[3] - results[1],
        results[0] - results[1],
    );
    Ok(())
}

/// Two temporal layers: are the upper-layer frames non-reference (droppable by
/// an SFU without a keyframe), and does the remaining base layer decode?
fn temporal_layers(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<(), Error> {
    let (width, height) = (1920u32, 1080u32);
    let scene = Scene::new(device, width, height)?;
    let processor = gpu::Processor::new(device, width, height)?;
    let quality = Quality { height, fps: 60 };
    let (mut encoder, accepted) = HardwareEncoder::new_gpu_with_codec_values(
        width as usize,
        height as usize,
        quality,
        device,
        EncoderOptions::default(),
        &[(CODECAPI_AVEncVideoTemporalLayerCount, 2)],
    )?;
    let readback = encoder.codec_value(&CODECAPI_AVEncVideoTemporalLayerCount);
    let mut stream = Vec::new();
    for frame in 0..60u64 {
        scene.draw(context, frame)?;
        let surface = processor
            .process(&scene.texture, 3.0, true)?
            .ok_or("no NV12 surface")?;
        stream.push(encode_one(&mut encoder, surface, frame * 16_667)?);
    }
    let non_reference = stream
        .iter()
        .filter(|data| slices(data).any(|(kind, nri)| matches!(kind, 1 | 5) && nri == 0))
        .count();
    let prefix = stream
        .iter()
        .filter(|data| slices(data).any(|(kind, _)| kind == 14))
        .count();
    let mut full = Decoder::new(width, height)?;
    let mut decoded_full = 0;
    for (i, data) in stream.iter().enumerate() {
        decoded_full += full.decode(Some((data, i as i64 * 166_667)))?.len();
    }
    decoded_full += full.decode(None)?.len();
    // Drop every non-reference access unit, as an SFU would for a weak viewer.
    let mut base = Decoder::new(width, height)?;
    let (mut kept, mut decoded_base, mut base_errors) = (0, 0, 0);
    for (i, data) in stream.iter().enumerate() {
        if slices(data).any(|(kind, nri)| matches!(kind, 1 | 5) && nri == 0) {
            continue;
        }
        kept += 1;
        match base.decode(Some((data, i as i64 * 166_667))) {
            Ok(pictures) => decoded_base += pictures.len(),
            Err(_) => base_errors += 1,
        }
    }
    decoded_base += base.decode(None).map_or(0, |p| p.len());
    println!(
        "Temporal layers: requested=2 accepted={accepted:?} readback={readback:?} profile={}; non_reference_frames={non_reference}/60 prefix_nal_frames={prefix}; full_decode={decoded_full}/60; base_layer_kept={kept} decoded={decoded_base} errors={base_errors}",
        encoder.profile
    );
    Ok(())
}

/// One long-term reference: mark the first frame, then predict a later frame
/// from it (the loss-recovery alternative to an IDR). Compares that frame's size
/// with a forced IDR and checks the whole stream still decodes.
fn long_term_reference(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<(), Error> {
    let (width, height) = (1920u32, 1080u32);
    let scene = Scene::new(device, width, height)?;
    let processor = gpu::Processor::new(device, width, height)?;
    // High word: trust mode (1: until told otherwise, 0: default); low: 1 LTR.
    for control in [(1u32 << 16) | 1, 1] {
        long_term_reference_with(device, context, &scene, &processor, control)?;
    }
    Ok(())
}
fn long_term_reference_with(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    scene: &Scene,
    processor: &gpu::Processor,
    control: u32,
) -> Result<(), Error> {
    let (width, height) = (1920u32, 1080u32);
    let quality = Quality { height, fps: 60 };
    let (mut encoder, accepted) = HardwareEncoder::new_gpu_with_codec_values(
        width as usize,
        height as usize,
        quality,
        device,
        EncoderOptions::default(),
        &[(CODECAPI_AVEncVideoLTRBufferControl, control)],
    )?;
    let readback = encoder.codec_value(&CODECAPI_AVEncVideoLTRBufferControl);
    let mut decoder = Decoder::new(width, height)?;
    let (mut sizes, mut decoded, mut marked, mut used) = (Vec::new(), 0, None, None);
    for frame in 0..60u64 {
        scene.draw(context, frame)?;
        let surface = processor
            .process(&scene.texture, 3.0, true)?
            .ok_or("no NV12 surface")?;
        if frame == 1 {
            marked = Some(
                encoder
                    .set_codec_value(&CODECAPI_AVEncVideoMarkLTRFrame, 0)
                    .is_ok(),
            );
        }
        if frame == 30 {
            // Bitmap of LTR slots the next frame may reference (slot 0).
            used = Some(
                encoder
                    .set_codec_value(&CODECAPI_AVEncVideoUseLTRFrame, 1)
                    .is_ok(),
            );
        }
        if frame == 45 {
            encoder.force_keyframe()?;
        }
        let data = encode_one(&mut encoder, surface, frame * 16_667)?;
        sizes.push(data.len());
        decoded += decoder.decode(Some((&data, frame as i64 * 166_667)))?.len();
    }
    decoded += decoder.decode(None)?.len();
    let deltas = sizes[2..30].iter().sum::<usize>() / 28;
    println!(
        "Long-term reference: control={control:#x} accepted={accepted:?} readback={readback:?} mark_ok={marked:?} use_ok={used:?}; mean_delta_bytes={deltas} ltr_recovery_frame_bytes={} forced_idr_bytes={} decoded={decoded}/60",
        sizes[30], sizes[45]
    );
    Ok(())
}
