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

/// GPU execution time (timestamp queries) of the capture copy plus conversion
/// to NV12, FP16 tone-map path versus the SDR single-blit path. No encoder:
/// this isolates the memory-bandwidth work that competes with a shared game.
pub fn conversion(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (3840u32, 2160u32);
    let query = |kind| -> Result<ID3D11Query, Box<dyn std::error::Error>> {
        let mut query = None;
        unsafe {
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: kind,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )?;
        }
        Ok(query.ok_or("query")?)
    };
    for (output_w, output_h) in [(3840u32, 2160u32), (1920, 1080), (1280, 720)] {
        let mut results = Vec::new();
        for format in [gpu::SourceFormat::Hdr, gpu::SourceFormat::Sdr] {
            let scene = super::scene::Scene::with_format(device, width, height, format.dxgi())?;
            scene.draw(context, 7)?;
            let captured = gpu::texture(device, width, height, format.dxgi(), format.bind_flags())?;
            let processor = match format {
                gpu::SourceFormat::Hdr => gpu::Processor::new(device, output_w, output_h)?,
                gpu::SourceFormat::Sdr => {
                    gpu::Processor::new_sdr(device, (width, height), output_w, output_h)?
                }
            };
            let (disjoint, begin, end) = (
                query(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                query(D3D11_QUERY_TIMESTAMP)?,
                query(D3D11_QUERY_TIMESTAMP)?,
            );
            let mut samples = Vec::new();
            let mut held = Vec::new();
            for _ in 0..240 {
                unsafe {
                    {
                        let _guard = gpu::ContextGuard::lock(context)?;
                        context.Begin(&disjoint);
                        context.End(&begin);
                        context.CopyResource(&captured, &scene.texture);
                    }
                    let surface = processor
                        .process(&captured, 3.0, true)?
                        .ok_or("no NV12 surface")?;
                    let _guard = gpu::ContextGuard::lock(context)?;
                    context.End(&end);
                    context.End(&disjoint);
                    // windows-rs maps the pending S_FALSE to Ok; data is only
                    // written once ready, so wait for a non-zero frequency.
                    let mut timing = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while timing.Frequency == 0 {
                        let _ = context.GetData(
                            &disjoint,
                            Some((&mut timing as *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT).cast()),
                            std::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                            0,
                        );
                        if Instant::now() > deadline {
                            return Err("GPU timestamp query timed out".into());
                        }
                        std::thread::yield_now();
                    }
                    let (mut start, mut stop) = (0u64, 0u64);
                    context.GetData(&begin, Some((&mut start as *mut u64).cast()), 8, 0)?;
                    context.GetData(&end, Some((&mut stop as *mut u64).cast()), 8, 0)?;
                    if !timing.Disjoint.as_bool() && timing.Frequency > 0 {
                        samples.push((stop - start) as f64 * 1000.0 / timing.Frequency as f64);
                    }
                    // Release NV12 surfaces; the encoder normally retires them.
                    held.push(surface);
                    held.clear();
                }
            }
            samples.sort_by(f64::total_cmp);
            let mean = samples.iter().sum::<f64>() / samples.len().max(1) as f64;
            let bytes_per_pixel = match format {
                gpu::SourceFormat::Hdr => 8,
                gpu::SourceFormat::Sdr => 4,
            };
            println!(
                "Conversion GPU time: {width}x{height} -> {output_w}x{output_h} {format:?}: mean_ms={mean:.3} p95_ms={:.3} samples={} capture_texture_MB={:.1}",
                samples
                    .get(samples.len() * 95 / 100)
                    .copied()
                    .unwrap_or_default(),
                samples.len(),
                f64::from(width * height * bytes_per_pixel) / 1e6
            );
            results.push(mean);
        }
        println!(
            "Conversion GPU time: {output_w}x{output_h} SDR single blit is {:.2}x faster than FP16 tone map",
            results[0] / results[1].max(1e-6)
        );
    }
    Ok(())
}
