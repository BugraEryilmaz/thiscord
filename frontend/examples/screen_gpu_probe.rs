//! Synthetic FP16 input only. No desktop capture, accounts or network.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use thiscord_frontend::screen::{gpu, hardware::HardwareEncoder};
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{Direct3D::*, Direct3D11::*, Dxgi::Common::*},
    };
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.unwrap();
        let context = context.unwrap();
        let _guard = gpu::ContextGuard::lock(&context)?;
        drop(_guard);
        for monitor in windows_capture::monitor::Monitor::enumerate()? {
            let (white, hdr) = gpu::display_white(windows::Win32::Graphics::Gdi::HMONITOR(
                monitor.as_raw_hmonitor(),
            ))?;
            println!(
                "Display metadata: HDR={hdr}, SDR white={} nits",
                white * 80.0
            );
        }
        if std::env::args().any(|arg| arg == "--burst") {
            return burst_probe(&device, &context);
        }
        if std::env::args().any(|arg| arg == "--adaptive") {
            return adaptive_probe(&device, &context);
        }
        for (w, h, source_w, source_h, white, hdr, value, low, high) in [
            (1280, 720, 1280, 720, 1.0, false, 0x3800u16, 170.0, 195.0),
            (1920, 1080, 2560, 1440, 2.0, true, 0x3c00u16, 170.0, 195.0),
            (1280, 720, 3840, 2160, 2.0, true, 0x4400u16, 248.0, 255.1),
        ] {
            let texture = gpu::texture(
                &device,
                source_w,
                source_h,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                D3D11_BIND_SHADER_RESOURCE.0 as u32,
            )?;
            let mut pixels = vec![value; (source_w * source_h * 4) as usize];
            for p in pixels.chunks_mut(4) {
                p[3] = 0x3c00;
            }
            context.UpdateSubresource(&texture, 0, None, pixels.as_ptr().cast(), source_w * 8, 0);
            let processor = gpu::Processor::new(&device, w, h)?;
            let quality = thiscord_shared::screen::Quality { height: h, fps: 60 };
            let mut encoder = HardwareEncoder::new_gpu(w as usize, h as usize, quality, &device)?;
            println!(
                "GPU path: {}; {source_w}x{source_h} ? {w}x{h}; white scale={white}, HDR={hdr}",
                encoder.name
            );
            let mut decoder = openh264::decoder::Decoder::new()?;
            let mut submitted = 0;
            let mut decoded = 0;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(8) && decoded < 120 {
                let mut frames = encoder.poll()?;
                if submitted < 120
                    && encoder.ready()
                    && let Some(nv12) = processor.process(&texture, white, hdr)?
                {
                    if submitted % 60 == 0 {
                        encoder.force_keyframe()?;
                    }
                    frames.extend(encoder.encode_texture(Arc::clone(&nv12), submitted * 16_667)?);
                    submitted += 1;
                }
                for frame in frames {
                    let image = decoder
                        .decode(&frame.data)?
                        .ok_or("Missing decoded frame")?;
                    let mut rgb = vec![0u8; (w * h * 3) as usize];
                    image.write_rgb8(&mut rgb);
                    // Both inputs normalize to linear 0.5, approximately 180 in BT.709.
                    let mean = rgb.iter().map(|v| *v as f64).sum::<f64>() / rgb.len() as f64;
                    if !(low..high).contains(&mean) {
                        return Err(format!("Unexpected tone-mapped level {mean}").into());
                    }
                    decoded += 1;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if decoded != 120 {
                return Err(
                    format!("GPU stalled: {submitted} submitted, {decoded} decoded").into(),
                );
            }
            println!("{decoded} GPU frames verified in {:?}", start.elapsed());
        }
    }
    Ok(())
}

/// Exercise encoder replacement and the low-rate admission limit on synthetic
/// surfaces. One decoder must recover across every resolution/FPS transition.
#[cfg(windows)]
fn adaptive_probe(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    use openh264::formats::YUVSource;
    use std::time::{Duration, Instant};
    use thiscord_frontend::screen::{
        gpu, hardware::HardwareEncoder, recovery_frame, sender_queue_frames,
    };
    use thiscord_shared::screen::Quality;
    use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};
    let texture = gpu::texture(
        device,
        1920,
        1080,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE.0 as u32,
    )?;
    let pixels = vec![0x3800_u16; 1920 * 1080 * 4];
    unsafe {
        context.UpdateSubresource(&texture, 0, None, pixels.as_ptr().cast(), 1920 * 8, 0);
    }
    let mut decoder = openh264::decoder::Decoder::new()?;
    let started = Instant::now();
    for quality in [
        Quality {
            height: 1080,
            fps: 30,
        },
        Quality {
            height: 720,
            fps: 15,
        },
        Quality {
            height: 360,
            fps: 5,
        },
        Quality {
            height: 1080,
            fps: 30,
        },
    ] {
        let (w, h) = (quality.width(), quality.height);
        let processor = gpu::Processor::new(device, w, h)?;
        let mut encoder = HardwareEncoder::new_gpu(w as usize, h as usize, quality, device)?;
        let (mut submitted, mut decoded) = (0, 0);
        let step = Instant::now();
        let mut next = step;
        while step.elapsed() < Duration::from_secs(8) && decoded < 10 {
            let mut frames = encoder.poll()?;
            if submitted < 10
                && Instant::now() >= next
                && encoder.ready()
                && encoder.pending() < sender_queue_frames(quality.fps)
                && let Some(surface) = processor.process(&texture, 1.0, false)?
            {
                frames
                    .extend(encoder.encode_texture(surface, started.elapsed().as_micros() as u64)?);
                submitted += 1;
                next = Instant::now() + Duration::from_secs_f64(1.0 / f64::from(quality.fps));
            }
            for frame in frames {
                if decoded == 0 {
                    assert!(
                        recovery_frame(&frame.data),
                        "reconfiguration must start with SPS/PPS and IDR"
                    );
                }
                let picture = decoder
                    .decode(&frame.data)?
                    .ok_or("Missing adaptive frame")?;
                assert_eq!(picture.dimensions(), (w as usize, h as usize));
                decoded += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            decoded, 10,
            "adaptive encoder stalled at {h}p/{}",
            quality.fps
        );
        println!(
            "Adaptive GPU: {w}x{h}/{} fps, {} bit/s, {decoded} decoded",
            quality.fps,
            quality.bitrate()
        );
    }
    Ok(())
}

/// Detailed synthetic scenes and repeated IDRs exercise rate control. This is
/// not a capture benchmark: CPU texture uploads and decoding are probe-only.
#[cfg(windows)]
fn burst_probe(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};
    use thiscord_frontend::screen::{gpu, hardware, recovery_frame};
    use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};
    let (w, h) = (2560, 1070);
    let quality = thiscord_shared::screen::Quality {
        height: 1440,
        fps: 60,
    };
    let texture = gpu::texture(
        device,
        w,
        h,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE.0 as u32,
    )?;
    let processor = gpu::Processor::new(device, w, h)?;
    let mut encoder = hardware::HardwareEncoder::new_gpu(w as usize, h as usize, quality, device)?;
    println!(
        "Burst probe: {}; target={} bytes, applied={}, readback={:?}",
        encoder.name,
        hardware::buffer_bytes(quality.bitrate()),
        encoder.buffer_applied,
        encoder.buffer_readback_bytes()
    );
    let mut decoder = openh264::decoder::Decoder::new()?;
    let (mut submitted, mut decoded, mut keyframes) = (0u64, 0, 0);
    let (mut peak, mut total) = (0, 0);
    let start = Instant::now();
    let mut pixels = vec![0u16; (w * h * 4) as usize];
    while start.elapsed() < Duration::from_secs(15) && decoded < 180 {
        let mut frames = encoder.poll()?;
        if submitted < 180
            && encoder.ready()
            && start.elapsed().as_micros() >= u128::from(submitted * 16_667)
        {
            if submitted.is_multiple_of(60) {
                // Deterministic textured 4x4 blocks, replaced once per second.
                let mut seed = submitted as u32 + 1;
                for y in (0..h).step_by(4) {
                    for x in (0..w).step_by(4) {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        let value = 0x3000 + (seed % 0x1000) as u16;
                        for row in y..(y + 4).min(h) {
                            for col in x..(x + 4).min(w) {
                                let offset = ((row * w + col) * 4) as usize;
                                pixels[offset..offset + 4]
                                    .copy_from_slice(&[value, value, value, 0x3c00]);
                            }
                        }
                    }
                }
                unsafe {
                    context.UpdateSubresource(&texture, 0, None, pixels.as_ptr().cast(), w * 8, 0);
                }
                encoder.force_keyframe()?;
            }
            if let Some(nv12) = processor.process(&texture, 3.0, true)? {
                frames.extend(encoder.encode_texture(nv12, submitted * 16_667)?);
                submitted += 1;
            }
        }
        for frame in frames {
            peak = peak.max(frame.data.len());
            total += frame.data.len();
            if recovery_frame(&frame.data) {
                keyframes += 1;
                println!("IDR bytes: {}", frame.data.len());
            }
            decoder
                .decode(&frame.data)?
                .ok_or("Missing burst-probe decoded frame")?;
            decoded += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if decoded != 180 || keyframes < 3 {
        return Err(format!(
            "Burst probe incomplete: submitted={submitted}, decoded={decoded}, IDRs={keyframes}"
        )
        .into());
    }
    println!("{decoded} detailed GPU frames decoded, peak={peak} bytes, total={total} bytes");
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    println!("D3D11 GPU probe requires Windows");
}
