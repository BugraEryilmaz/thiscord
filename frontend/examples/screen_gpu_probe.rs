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
#[cfg(not(windows))]
fn main() {
    println!("D3D11 GPU probe requires Windows");
}
