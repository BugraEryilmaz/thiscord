//! Synthetic two-encoder acceptance: one capture texture, independently encoded
//! qualities, and repeated low-layer teardown while the primary stays alive.
use openh264::formats::YUVSource;
use std::time::{Duration, Instant};
use thiscord_frontend::screen::{gpu, hardware::HardwareEncoder, recovery_frame};
use thiscord_shared::screen::Quality;
use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};

pub fn run(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = gpu::texture(
        device,
        1920,
        1080,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_SHADER_RESOURCE.0 as u32,
    )?;
    let pixels = vec![0x3800_u16; 1920 * 1080 * 4];
    unsafe {
        context.UpdateSubresource(&raw, 0, None, pixels.as_ptr().cast(), 1920 * 8, 0);
    }
    let primary_quality = Quality::default();
    let primary_processor = gpu::Processor::new(device, 1920, 1080)?;
    let mut primary = HardwareEncoder::new_gpu(1920, 1080, primary_quality, device)?;
    let mut primary_decoder = openh264::decoder::Decoder::new()?;
    let started = Instant::now();
    for low_quality in [
        Some(Quality {
            height: 360,
            fps: 5,
        }),
        None,
        Some(Quality {
            height: 720,
            fps: 15,
        }),
        None,
    ] {
        let mut low = low_quality
            .map(|q| -> Result<_, windows::core::Error> {
                Ok((
                    gpu::Processor::new(device, q.width(), q.height)?,
                    HardwareEncoder::new_gpu(q.width() as usize, q.height as usize, q, device)?,
                ))
            })
            .transpose()?;
        let mut low_decoder = openh264::decoder::Decoder::new()?;
        primary.force_keyframe()?;
        let phase = Instant::now();
        let mut next = [phase; 2];
        let mut counts = [0; 2];
        let mut submitted = [0; 2];
        while phase.elapsed() < Duration::from_secs(8)
            && (counts[0] < 30 || (low.is_some() && counts[1] < 10))
        {
            for index in 0..2 {
                let (processor, encoder, quality, decoder) = if index == 0 {
                    (
                        &primary_processor,
                        &mut primary,
                        primary_quality,
                        &mut primary_decoder,
                    )
                } else if let Some((processor, encoder)) = &mut low {
                    (&*processor, encoder, low_quality.unwrap(), &mut low_decoder)
                } else {
                    continue;
                };
                let target = if index == 0 { 30 } else { 10 };
                let mut frames = encoder.poll()?;
                if submitted[index] < target
                    && Instant::now() >= next[index]
                    && encoder.ready()
                    && let Some(surface) = processor.process(&raw, 1.0, false)?
                {
                    frames.extend(
                        encoder.encode_texture(surface, started.elapsed().as_micros() as u64)?,
                    );
                    submitted[index] += 1;
                    next[index] =
                        Instant::now() + Duration::from_secs_f64(1.0 / f64::from(quality.fps));
                }
                for frame in frames {
                    if counts[index] == 0 {
                        assert!(recovery_frame(&frame.data));
                    }
                    let image = decoder
                        .decode(&frame.data)?
                        .ok_or("Layer frame did not decode")?;
                    assert_eq!(
                        image.dimensions(),
                        (quality.width() as usize, quality.height as usize)
                    );
                    counts[index] += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(counts, [30, if low.is_some() { 10 } else { 0 }]);
        println!(
            "Concurrent GPU layers: primary=1080p/30, low={low_quality:?}, decoded={counts:?}, elapsed={:?}",
            phase.elapsed()
        );
    }
    Ok(())
}
