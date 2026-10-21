//! Paired deterministic GPU fidelity/recovery probe. Readback/software decode
//! are diagnostic only; the elapsed runtime is not a throughput measurement.
use super::scene::Scene;
use openh264::formats::YUVSource;
use std::time::{Duration, Instant};
use thiscord_frontend::screen::{gpu, hardware::HardwareEncoder, keyframe, recovery_frame};
use thiscord_shared::screen::Quality;
use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};

pub fn run(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height, fps) = (1920u32, 1080u32, 60u32);
    let quality = Quality { height, fps };
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
    let mut reference = vec![0u8; (width * height) as usize];
    // Keep speed, bitrate, frame timestamps and synthetic input identical.
    for (label, gop, buffer_ms) in [
        ("legacy", fps, 100),
        ("long_gop", keyframe::gop_frames(fps), 100),
        ("production", keyframe::gop_frames(fps), 200),
    ] {
        let mut encoder = HardwareEncoder::new_gpu_with_recovery_targets(
            width as usize,
            height as usize,
            quality,
            device,
            gop,
            buffer_ms,
        )?;
        println!(
            "Policy {label}: encoder={}, bitrate={}, GOP target={gop} applied={} readback={:?}, HRD ms={buffer_ms} applied={} bytes={:?}",
            encoder.name,
            quality.bitrate(),
            encoder.gop_applied,
            encoder.gop_readback_frames(),
            encoder.buffer_applied,
            encoder.buffer_readback_bytes(),
        );
        let mut decoder = openh264::decoder::Decoder::new()?;
        let (mut total_error, mut total_bytes, mut peak) = (0u64, 0usize, 0usize);
        let mut idrs = Vec::new();
        let mut last_idr = 0;
        let mut scores = Vec::new();
        let mut recovered = false;
        for frame in 0..360u64 {
            scene.draw(context, frame)?;
            let surface = processor
                .process(&scene.texture, 3.0, true)?
                .ok_or("No free probe NV12 surface")?;
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
                if !encoder.poll()?.is_empty() {
                    return Err("Unexpected unaccounted probe output".into());
                }
                if Instant::now() > deadline {
                    return Err("Encoder input timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            // Reproduce legacy one-second forced IDRs, then inject the same
            // on-demand request into every arm midway between periodic IDRs.
            if frame.saturating_sub(last_idr) >= u64::from(gop) || frame == 210 {
                encoder.force_keyframe()?;
            }
            let mut output = encoder.encode_texture(surface, frame * 1_000_000 / u64::from(fps))?;
            while output.is_empty() {
                if Instant::now() > deadline {
                    return Err("Encoder output timeout".into());
                }
                std::thread::sleep(Duration::from_millis(1));
                output.extend(encoder.poll()?);
            }
            if output.len() != 1 {
                return Err("Expected one output per submitted input".into());
            }
            let encoded = &output[0];
            if encoded.timestamp != frame * 1_000_000 / u64::from(fps) {
                return Err("Unexpected reordered output timestamp".into());
            }
            let idr = recovery_frame(&encoded.data);
            if idr {
                idrs.push(frame);
                last_idr = frame;
            }
            if frame == 210 {
                // A new/lost viewer has no old reference pictures or parameter
                // sets. Decode the requested recovery using a fresh decoder.
                if !idr {
                    return Err("On-demand recovery did not produce SPS/PPS/IDR".into());
                }
                decoder = openh264::decoder::Decoder::new()?;
                recovered = true;
            }
            total_bytes += encoded.data.len();
            peak = peak.max(encoded.data.len());
            let image = decoder
                .decode(&encoded.data)?
                .ok_or("Missing decoded probe frame")?;
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
            total_error += error;
            scores.push(psnr(error, u64::from(width) * u64::from(height)));
        }
        if !recovered || idrs.first() != Some(&0) {
            return Err("Recovery coverage incomplete".into());
        }
        // Expose overrides as an explicit failure, rather than reporting a
        // longer GOP when the hardware still injects one-second keyframes.
        if label != "legacy" && idrs != [0, 210] {
            return Err(
                format!("Driver did not honor long GOP on synthetic scene: {idrs:?}").into(),
            );
        }
        println!(
            "Policy {label}: decoded=360 IDRs={idrs:?} luma_PSNR_dB={:.3} worst_frame_dB={:.3} peak_bytes={peak} nominal_Mbps={:.3} fresh_decoder_recovery_frame=210",
            psnr(total_error, u64::from(width) * u64::from(height) * 360),
            scores.iter().copied().fold(f64::INFINITY, f64::min),
            total_bytes as f64 * 8.0 / 6.0 / 1e6,
        );
        for second in 1..6 {
            let boundary = second * fps as usize;
            let first = scores[boundary..boundary + 10].iter().sum::<f64>() / 10.0;
            let preceding = scores[boundary - 10..boundary].iter().sum::<f64>() / 10.0;
            println!(
                "Policy {label}: second={second} preceding_10_mean_dB={preceding:.3} following_10_mean_dB={first:.3}"
            );
        }
    }
    Ok(())
}

fn psnr(error: u64, samples: u64) -> f64 {
    10.0 * (255.0f64.powi(2) * samples as f64 / error.max(1) as f64).log10()
}
