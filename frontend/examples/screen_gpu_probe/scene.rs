use thiscord_frontend::screen::gpu;
use windows::{
    Win32::Graphics::{Direct3D::Fxc::*, Direct3D::*, Direct3D11::*, Dxgi::Common::*},
    core::s,
};

pub struct Scene {
    pub texture: ID3D11Texture2D,
    target: ID3D11RenderTargetView,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    constants: ID3D11Buffer,
    width: u32,
    height: u32,
}
impl Scene {
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> windows::core::Result<Self> {
        let texture = gpu::texture(
            device,
            width,
            height,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        )?;
        unsafe {
            let source = include_str!("scene.hlsl");
            let compile =
                |entry, target| -> windows::core::Result<Vec<u8>> {
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
            let (mut vertex, mut pixel, mut target, mut constants) = (None, None, None, None);
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
            device.CreateRenderTargetView(&texture, None, Some(&mut target))?;
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
            Ok(Self {
                texture,
                target: target.unwrap(),
                vertex: vertex.unwrap(),
                pixel: pixel.unwrap(),
                constants: constants.unwrap(),
                width,
                height,
            })
        }
    }
    pub fn draw(&self, context: &ID3D11DeviceContext, frame: u64) -> windows::core::Result<()> {
        let _guard = gpu::ContextGuard::lock(context)?;
        unsafe {
            let parameters = [frame as f32, self.width as f32, self.height as f32, 0.0];
            context.UpdateSubresource(&self.constants, 0, None, parameters.as_ptr().cast(), 0, 0);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.OMSetRenderTargets(Some(&[Some(self.target.clone())]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: self.width as f32,
                Height: self.height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]));
            context.Draw(3, 0);
            context.OMSetRenderTargets(None, None);
        }
        Ok(())
    }
}
