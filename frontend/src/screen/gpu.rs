//! FP16 scRGB -> scaled/tone-mapped BT.709 RGB -> NV12, entirely on D3D11.
//! Capture, processor and encoder share one multithread-protected device.
use std::{
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

pub struct Processor {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    target: ID3D11RenderTargetView,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    outputs: Vec<Arc<ID3D11Texture2D>>,
    width: u32,
    height: u32,
}
impl Processor {
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let context = device.GetImmediateContext()?;
            let _guard = ContextGuard::lock(&context)?;
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
            let video: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext1 = context.cast()?;
            let enumerator =
                video.CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputWidth: width,
                    InputHeight: height,
                    OutputWidth: width,
                    OutputHeight: height,
                    Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                    ..Default::default()
                })?;
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
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
            let mut input = None;
            video.CreateVideoProcessorInputView(
                &rgb,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut input),
            )?;
            let outputs = (0..6)
                .map(|_| {
                    texture(
                        device,
                        width,
                        height,
                        DXGI_FORMAT_NV12,
                        D3D11_BIND_RENDER_TARGET.0 as u32,
                    )
                    .map(Arc::new)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Self {
                device: device.clone(),
                context,
                vertex: vertex.unwrap(),
                pixel: pixel.unwrap(),
                sampler: sampler.unwrap(),
                constants: constants.unwrap(),
                target: target.unwrap(),
                video,
                video_context,
                enumerator,
                processor,
                input: input.unwrap(),
                outputs,
                width,
                height,
            })
        }
    }
    /// None means all bounded output surfaces are still owned by the encoder.
    pub fn process(
        &self,
        source: &ID3D11Texture2D,
        white: f32,
        hdr: bool,
    ) -> Result<Option<Arc<ID3D11Texture2D>>> {
        let Some(output) = self.outputs.iter().find(|t| Arc::strong_count(t) == 1) else {
            return Ok(None);
        };
        unsafe {
            let _guard = ContextGuard::lock(&self.context)?;
            let mut view = None;
            self.device
                .CreateShaderResourceView(source, None, Some(&mut view))?;
            let parameters = [white.max(1.0), if hdr { 1.0 } else { 0.0 }, 0.0, 0.0];
            self.context.UpdateSubresource(
                &self.constants,
                0,
                None,
                parameters.as_ptr().cast(),
                0,
                0,
            );
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vertex, None);
            self.context.PSSetShader(&self.pixel, None);
            self.context.PSSetShaderResources(0, Some(&[view]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            self.context
                .OMSetRenderTargets(Some(&[Some(self.target.clone())]), None);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: self.width as f32,
                Height: self.height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]));
            self.context.Draw(3, 0);
            self.context.PSSetShaderResources(0, Some(&[None]));
            self.context.OMSetRenderTargets(None, None);
            let mut target = None;
            self.video.CreateVideoProcessorOutputView(
                output.as_ref(),
                &self.enumerator,
                &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut target),
            )?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(self.input.clone())),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                target.as_ref().unwrap(),
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
