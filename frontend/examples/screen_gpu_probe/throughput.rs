use super::scene::Scene;
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
    let (width, height) = (3840, 2160);
    let scene = Scene::new(device, width, height)?;
    // Source rendering belongs to the shared application. Pre-render a short
    // moving sequence so this benchmark measures capture-copy/conversion/encode,
    // not the synthetic shader's millions of sin() calls on an integrated GPU.
    let mut sources = Vec::new();
    for frame in 0..8 {
        scene.draw(context, frame)?;
        let texture = gpu::texture(
            device,
            width,
            height,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
        )?;
        unsafe {
            context.CopyResource(&texture, &scene.texture);
        }
        sources.push(texture);
    }
    unsafe {
        context.Flush();
    }
    let captured = gpu::texture(
        device,
        width,
        height,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE.0 as u32,
    )?;
    let processor = gpu::Processor::new(device, width, height)?;
    for paced in [true, false] {
        let quality = Quality { height, fps: 60 };
        let mut encoder =
            HardwareEncoder::new_gpu(width as usize, height as usize, quality, device)?;
        println!("Quality/speed readback: {:?}", encoder.quality_vs_speed());
        let started = Instant::now();
        let mut submitted = 0u64;
        let mut output = 0u64;
        let mut bytes = 0u64;
        let mut latency = Vec::new();
        let mut cpu = Vec::new();
        let frames = 600;
        while output < frames && started.elapsed() < Duration::from_secs(25) {
            let mut encoded = encoder.poll()?;
            let due = !paced || started.elapsed().as_secs_f64() >= submitted as f64 / 60.0;
            if submitted < frames && due && encoder.ready() {
                let at = Instant::now();
                unsafe {
                    let _guard = gpu::ContextGuard::lock(context)?;
                    context.CopyResource(&captured, &sources[submitted as usize % sources.len()]);
                    context.Flush();
                }
                if let Some(surface) = processor.process(&captured, 3.0, true)? {
                    if submitted.is_multiple_of(60) {
                        encoder.force_keyframe()?;
                    }
                    encoded.extend(encoder.encode_texture(
                        Arc::clone(&surface),
                        started.elapsed().as_micros() as u64,
                    )?);
                    cpu.push(at.elapsed().as_secs_f64() * 1000.0);
                    submitted += 1;
                }
            }
            for frame in encoded {
                latency.push(frame.encoding_duration.as_secs_f64() * 1000.0);
                bytes += frame.data.len() as u64;
                output += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let seconds = started.elapsed().as_secs_f64();
        latency.sort_by(f64::total_cmp);
        cpu.sort_by(f64::total_cmp);
        let p95 = |values: &[f64]| {
            values
                .get(values.len() * 95 / 100)
                .copied()
                .unwrap_or_default()
        };
        println!(
            "4K GPU pipeline: mode={}, encoder={}, submitted={}, encoded={}, seconds={seconds:.3}, fps={:.2}, encoded_Mbps={:.2}, encoder_mean_ms={:.3}, encoder_p95_ms={:.3}, submit_p95_ms={:.3}",
            if paced { "60fps" } else { "headroom" },
            encoder.name,
            submitted,
            output,
            output as f64 / seconds,
            bytes as f64 * 8.0 / seconds / 1e6,
            latency.iter().sum::<f64>() / latency.len().max(1) as f64,
            p95(&latency),
            p95(&cpu)
        );
        if output != frames {
            return Err("4K pipeline failed to drain".into());
        }
    }
    Ok(())
}
