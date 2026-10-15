//! Offline fidelity comparison only. Readback/software decode are deliberately
//! outside the production path and must not be included in throughput figures.
use super::scene::Scene;
use openh264::formats::YUVSource;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use thiscord_frontend::screen::{gpu, hardware::HardwareEncoder};
use thiscord_shared::screen::Quality;
use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};

pub fn run(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (3840u32, 2160u32);
    let scene = Scene::new(device, width, height)?;
    let processor = gpu::Processor::new(device, width, height)?;
    let mut staging = None;
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
            Some(&mut staging),
        )?;
    }
    let staging = staging.unwrap();
    let mut scores = Vec::new();
    for speed in [None, Some(0), Some(16), Some(24)] {
        let mut encoder = HardwareEncoder::new_gpu_with_quality_speed(
            width as usize,
            height as usize,
            Quality { height, fps: 60 },
            device,
            speed,
        )?;
        let mut decoder = openh264::decoder::Decoder::new()?;
        let mut bytes = 0usize;
        let mut squared_error = 0u64;
        let mut samples = 0u64;
        let mut worst = f64::INFINITY;
        for frame in 0..120u64 {
            scene.draw(context, frame)?;
            let surface = processor
                .process(&scene.texture, 3.0, true)?
                .ok_or("No free NV12 surface")?;
            let mut reference = vec![0u8; (width * height) as usize];
            unsafe {
                let _guard = gpu::ContextGuard::lock(context)?;
                context.CopyResource(&staging, surface.as_ref());
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                for row in 0..height as usize {
                    reference[row * width as usize..(row + 1) * width as usize].copy_from_slice(
                        std::slice::from_raw_parts(
                            (mapped.pData as *const u8).add(row * mapped.RowPitch as usize),
                            width as usize,
                        ),
                    );
                }
                context.Unmap(&staging, 0);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !encoder.ready() {
                encoder.poll()?;
                if Instant::now() > deadline {
                    return Err("Encoder input timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut encoded = encoder.encode_texture(Arc::clone(&surface), frame * 16_667)?;
            while encoded.is_empty() {
                if Instant::now() > deadline {
                    return Err("Encoder output timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
                encoded.extend(encoder.poll()?);
            }
            assert_eq!(encoded.len(), 1);
            bytes += encoded[0].data.len();
            let image = decoder
                .decode(&encoded[0].data)?
                .ok_or("Missing decoded quality frame")?;
            let mut error = 0u64;
            for row in 0..height as usize {
                let actual =
                    &image.y()[row * image.strides().0..row * image.strides().0 + width as usize];
                for (a, b) in actual
                    .iter()
                    .zip(&reference[row * width as usize..(row + 1) * width as usize])
                {
                    error += (i32::from(*a) - i32::from(*b)).pow(2) as u64;
                }
            }
            squared_error += error;
            samples += u64::from(width) * u64::from(height);
            worst = worst.min(
                10.0 * (255.0f64.powi(2) * f64::from(width * height) / error.max(1) as f64).log10(),
            );
        }
        let psnr = 10.0 * (255.0f64.powi(2) * samples as f64 / squared_error.max(1) as f64).log10();
        scores.push(psnr);
        println!(
            "4K fidelity: requested={speed:?}, readback={:?}, luma_PSNR_dB={psnr:.2}, worst_frame_dB={worst:.2}, nominal_Mbps={:.2}",
            encoder.quality_vs_speed(),
            bytes as f64 * 8.0 / 2.0 / 1e6
        );
    }
    println!(
        "Speed hint luma PSNR change: {:.2} dB (synthetic content only)",
        scores[1] - scores[0]
    );
    Ok(())
}
