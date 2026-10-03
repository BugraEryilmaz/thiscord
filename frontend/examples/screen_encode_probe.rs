//! Explicit local hardware verification using synthetic pixels, never the screen.
#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
    use std::time::{Duration, Instant};
    use thiscord_frontend::screen::{bounded_parameter_sets, hardware::HardwareEncoder};
    use thiscord_shared::screen::Quality;
    for height in [720, 1080] {
        let quality = Quality { height, fps: 60 };
        let (w, h) = (quality.width() as usize, height as usize);
        let mut encoder = HardwareEncoder::new(w, h, quality)?;
        println!("{}x{}: hardware encoder {}", w, h, encoder.name);
        let mut rgba = vec![255; w * h * 4];
        let mut yuv = YUVBuffer::new(w, h);
        let mut decoded = openh264::decoder::Decoder::new()?;
        let start = Instant::now();
        let mut received = 0;
        let mut check = |frames: Vec<thiscord_frontend::screen::hardware::Encoded>| {
            for frame in frames {
                assert!(bounded_parameter_sets(&frame.data));
                if frame.timestamp == 0 || frame.timestamp == 1_000_000 {
                    assert!(
                        thiscord_frontend::screen::recovery_frame(&frame.data),
                        "forced hardware keyframes need SPS/PPS + IDR for late viewers"
                    );
                }
                if let Some(picture) = decoded
                    .decode(&frame.data)
                    .expect("hardware bitstream must decode")
                {
                    assert_eq!(picture.dimensions(), (w, h));
                    received += 1;
                }
            }
        };
        for n in 0..120_u64 {
            for row in rgba.chunks_mut(w * 4) {
                for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    pixel[0] = ((x + n as usize * 8) % 256) as u8;
                    pixel[1] = ((x / 4 + n as usize * 3) % 256) as u8;
                }
            }
            yuv.read_rgba8(RgbaSliceU8::new(&rgba, (w, h)));
            if n % 60 == 0 {
                encoder.force_keyframe()?;
            }
            check(encoder.encode(&yuv, n * 1_000_000 / 60)?);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            check(encoder.poll()?);
            std::thread::sleep(Duration::from_millis(2));
        }
        println!(
            "Decoded {received}/120 frames in {:.2}s (includes 2s final drain and software decode verification)",
            start.elapsed().as_secs_f64()
        );
        assert_eq!(
            received, 120,
            "hardware encoder must not silently stall or lose frames"
        );
    }
    Ok(())
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("The Media Foundation encoder probe requires Windows.");
}
