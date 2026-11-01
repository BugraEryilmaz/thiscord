//! Capture -> NV12, entirely on D3D11. SDR displays use one video-processor
//! blit from 8-bit BGRA; HDR displays tone-map FP16 scRGB to BT.709 first.
//! Capture, processor and encoder share one multithread-protected device.
use std::{
    cell::RefCell,
    mem::{ManuallyDrop, size_of},
    sync::Arc,
};
use windows::{
    Win32::{
        Devices::Display::*,
        Foundation::*,
        Graphics::{Direct3D::Fxc::*, Direct3D::*, Direct3D11::*, Dxgi::Common::*, Gdi::*},
    },
    core::{Interface, Result, s},
};

pub fn texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    bind: u32,
) -> Result<ID3D11Texture2D> {
    let mut result = None;
    unsafe {
        device.CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind,
                ..Default::default()
            },
            None,
            Some(&mut result),
        )?;
    }
    result.ok_or_else(windows::core::Error::from_thread)
}
pub struct ContextGuard(ID3D11Multithread);
impl ContextGuard {
    pub fn lock(context: &ID3D11DeviceContext) -> Result<Self> {
        let lock: ID3D11Multithread = context.cast()?;
        unsafe {
            let _ = lock.SetMultithreadProtected(true);
            lock.Enter();
        }
        Ok(Self(lock))
    }
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        unsafe {
            self.0.Leave();
        }
    }
}

/// HDR shader stage: FP16 scRGB -> tone-mapped BT.709 RGB intermediate.
struct ToneMap {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    target: ID3D11RenderTargetView,
    input: ID3D11VideoProcessorInputView,
    views: RefCell<ViewCache<ID3D11ShaderResourceView>>,
}

/// Views keyed by source texture identity. Capture reuses a small bounded pool,
/// so creating a view per frame was avoidable driver work. The cache holds COM
/// references only (not the pool's `Arc`), so pool ownership checks still work.
struct ViewCache<V> {
    entries: Vec<(ID3D11Texture2D, V)>,
}
impl<V: Clone> ViewCache<V> {
    const LIMIT: usize = 8;
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
    fn get(&mut self, texture: &ID3D11Texture2D, create: impl FnOnce() -> Result<V>) -> Result<V> {
        if let Some((_, view)) = self
            .entries
            .iter()
            .find(|(t, _)| t.as_raw() == texture.as_raw())
        {
            return Ok(view.clone());
        }
        let view = create()?;
        if self.entries.len() == Self::LIMIT {
            self.entries.remove(0);
        }
        self.entries.push((texture.clone(), view.clone()));
        Ok(view)
    }
}

/// Capture surface encoding chosen once per capture session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// Linear FP16 scRGB, required to tone-map an HDR display's highlights.
    Hdr,
    /// 8-bit BGRA from the SDR compositor. Scaling and RGB->NV12 happen in one
    /// video-processor blit: no shader pass, FP16 copy or BGRA intermediate.
    Sdr,
}
impl SourceFormat {
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Hdr => DXGI_FORMAT_R16G16B16A16_FLOAT,
            Self::Sdr => DXGI_FORMAT_B8G8R8A8_UNORM,
        }
    }
    /// Capture textures are video-processor inputs in SDR and shader inputs in HDR.
    pub fn bind_flags(self) -> u32 {
        match self {
            Self::Hdr => D3D11_BIND_SHADER_RESOURCE.0 as u32,
            Self::Sdr => (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        }
    }
}

pub struct Processor {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    tone_map: Option<ToneMap>,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    sdr_inputs: RefCell<ViewCache<ID3D11VideoProcessorInputView>>,
    outputs: Vec<(Arc<ID3D11Texture2D>, ID3D11VideoProcessorOutputView)>,
    source: (u32, u32),
    width: u32,
    height: u32,
}
impl Processor {
    /// HDR-capable FP16 path at the output size (existing probes and HDR capture).
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        Self::create(device, SourceFormat::Hdr, (width, height), width, height)
    }
    /// SDR fast path: one fixed-function blit scales `source` to `width`x`height`.
    pub fn new_sdr(
        device: &ID3D11Device,
        source: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::create(device, SourceFormat::Sdr, source, width, height)
    }
    pub fn format(&self) -> SourceFormat {
        if self.tone_map.is_some() {
            SourceFormat::Hdr
        } else {
            SourceFormat::Sdr
        }
    }
    pub fn source(&self) -> (u32, u32) {
        self.source
    }
    fn create(
        device: &ID3D11Device,
        format: SourceFormat,
        source: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<Self> {
        unsafe {
            let context = device.GetImmediateContext()?;
            let _guard = ContextGuard::lock(&context)?;
            let video: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext1 = context.cast()?;
            // HDR blits a same-size intermediate; SDR scales the capture itself.
            let input = match format {
                SourceFormat::Hdr => (width, height),
                SourceFormat::Sdr => source,
            };
            let enumerator =
                video.CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputWidth: input.0,
                    InputHeight: input.1,
                    OutputWidth: width,
                    OutputHeight: height,
                    Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                    ..Default::default()
                })?;
            if format == SourceFormat::Sdr {
                let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)?;
                if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
                    return Err(windows::core::Error::from_hresult(E_NOTIMPL));
                }
            }
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            // Both paths feed gamma-encoded BT.709-primary RGB to the blit.
            video_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            // Explicit full-surface rectangles: the SDR blit is the scaler.
            let input_rect = RECT {
                left: 0,
                top: 0,
                right: input.0 as i32,
                bottom: input.1 as i32,
            };
            let output_rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&input_rect));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&output_rect));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&output_rect));
            let tone_map = match format {
                SourceFormat::Hdr => {
                    Some(Self::tone_map(device, &video, &enumerator, width, height)?)
                }
                SourceFormat::Sdr => None,
            };
            let outputs = (0..6)
                .map(|_| {
                    let texture = texture(
                        device,
                        width,
                        height,
                        DXGI_FORMAT_NV12,
                        D3D11_BIND_RENDER_TARGET.0 as u32,
                    )?;
                    let mut view = None;
                    video.CreateVideoProcessorOutputView(
                        &texture,
                        &enumerator,
                        &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                            ..Default::default()
                        },
                        Some(&mut view),
                    )?;
                    Ok((
                        Arc::new(texture),
                        view.ok_or_else(windows::core::Error::from_thread)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Self {
                device: device.clone(),
                context,
                tone_map,
                video,
                video_context,
                enumerator,
                processor,
                sdr_inputs: RefCell::new(ViewCache::new()),
                outputs,
                source,
                width,
                height,
            })
        }
    }
    unsafe fn tone_map(
        device: &ID3D11Device,
        video: &ID3D11VideoDevice,
        enumerator: &ID3D11VideoProcessorEnumerator,
        width: u32,
        height: u32,
    ) -> Result<ToneMap> {
        unsafe {
            let source = include_str!("tonemap.hlsl");
            let compile =
                |entry, target| -> Result<Vec<u8>> {
                    let mut code = None;
                    D3DCompile(
                        source.as_ptr().cast(),
                        source.len(),
                        None,
                        None,
                        None,
                        entry,
                        target,
                        D3DCOMPILE_OPTIMIZATION_LEVEL3,
                        0,
                        &mut code,
                        None,
                    )?;
                    let code = code.unwrap();
                    Ok(std::slice::from_raw_parts(
                        code.GetBufferPointer().cast(),
                        code.GetBufferSize(),
                    )
                    .to_vec())
                };
            let mut vertex = None;
            let mut pixel = None;
            device.CreateVertexShader(
                &compile(s!("vertex"), s!("vs_5_0"))?,
                None,
                Some(&mut vertex),
            )?;
            device.CreatePixelShader(
                &compile(s!("pixel"), s!("ps_5_0"))?,
                None,
                Some(&mut pixel),
            )?;
            let mut sampler = None;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut constants = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut constants),
            )?;
            let rgb = texture(
                device,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_BIND_RENDER_TARGET.0 as u32,
            )?;
            let mut target = None;
            device.CreateRenderTargetView(&rgb, None, Some(&mut target))?;
            let mut input = None;
            video.CreateVideoProcessorInputView(
                &rgb,
                enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut input),
            )?;
            Ok(ToneMap {
                vertex: vertex.unwrap(),
                pixel: pixel.unwrap(),
                sampler: sampler.unwrap(),
                constants: constants.unwrap(),
                target: target.unwrap(),
                input: input.unwrap(),
                views: RefCell::new(ViewCache::new()),
            })
        }
    }
    /// None means all bounded output surfaces are still owned by the encoder.
    /// `white`/`hdr` only affect the FP16 tone-map path.
    pub fn process(
        &self,
        source: &ID3D11Texture2D,
        white: f32,
        hdr: bool,
    ) -> Result<Option<Arc<ID3D11Texture2D>>> {
        let Some((output, target)) = self.outputs.iter().find(|(t, _)| Arc::strong_count(t) == 1)
        else {
            return Ok(None);
        };
        unsafe {
            let _guard = ContextGuard::lock(&self.context)?;
            let input = if let Some(tone) = &self.tone_map {
                let view = tone.views.borrow_mut().get(source, || {
                    let mut view = None;
                    self.device
                        .CreateShaderResourceView(source, None, Some(&mut view))?;
                    view.ok_or_else(windows::core::Error::from_thread)
                })?;
                let parameters = [white.max(1.0), if hdr { 1.0 } else { 0.0 }, 0.0, 0.0];
                self.context.UpdateSubresource(
                    &tone.constants,
                    0,
                    None,
                    parameters.as_ptr().cast(),
                    0,
                    0,
                );
                self.context
                    .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                self.context.VSSetShader(&tone.vertex, None);
                self.context.PSSetShader(&tone.pixel, None);
                self.context.PSSetShaderResources(0, Some(&[Some(view)]));
                self.context
                    .PSSetSamplers(0, Some(&[Some(tone.sampler.clone())]));
                self.context
                    .PSSetConstantBuffers(0, Some(&[Some(tone.constants.clone())]));
                self.context
                    .OMSetRenderTargets(Some(&[Some(tone.target.clone())]), None);
                self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: self.width as f32,
                    Height: self.height as f32,
                    MaxDepth: 1.0,
                    ..Default::default()
                }]));
                self.context.Draw(3, 0);
                self.context.PSSetShaderResources(0, Some(&[None]));
                self.context.OMSetRenderTargets(None, None);
                tone.input.clone()
            } else {
                self.sdr_inputs.borrow_mut().get(source, || {
                    let mut view = None;
                    self.video.CreateVideoProcessorInputView(
                        source,
                        &self.enumerator,
                        &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                            ..Default::default()
                        },
                        Some(&mut view),
                    )?;
                    view.ok_or_else(windows::core::Error::from_thread)
                })?
            };
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input)),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                target,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
            self.context.Flush();
        }
        Ok(Some(output.clone()))
    }
}

/// Ask the scheduler to favor this device's small capture/convert workload over
/// a saturating game. Advisory: drivers may clamp or ignore it.
pub fn raise_gpu_priority(device: &ID3D11Device) -> &'static str {
    let applied = device
        .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()
        .and_then(|dxgi| unsafe { dxgi.SetGPUThreadPriority(7) });
    if applied.is_ok() { "7" } else { "default" }
}

/// Resolve the selected display's SDR brightness slider, not the primary display.
/// Windows reports SDR white in units of 80/1000 nits.
pub fn display_white(monitor: HMONITOR) -> Result<(f32, bool)> {
    unsafe {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO).ok()?;
        let (mut pc, mut mc) = (0, 0);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut pc, &mut mc).ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); pc as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mc as usize];
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut pc,
            paths.as_mut_ptr(),
            &mut mc,
            modes.as_mut_ptr(),
            None,
        )
        .ok()?;
        for path in paths.iter().take(pc as usize) {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0
                || name.viewGdiDeviceName != info.szDevice
            {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut color.header) != 0 {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            let hdr = color.Anonymous.value & 2 != 0;
            if !hdr {
                return Ok((1.0, false));
            }
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 || white.SDRWhiteLevel == 0 {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            return Ok((white.SDRWhiteLevel as f32 / 1000.0, true));
        }
        Err(windows::core::Error::from_hresult(E_FAIL))
    }
}
