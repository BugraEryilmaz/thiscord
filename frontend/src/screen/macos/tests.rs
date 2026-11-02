use super::*;
use crate::screen::{
    encode::EncoderOptions,
    worker::{LayerEncoder, Pixels},
};
use objc2_core_foundation::{CFDictionary, CFString, CFType};
use objc2_core_video::{
    CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use std::ptr::NonNull;

/// IOSurface-backed NV12 (video range) with a moving gradient.
fn surface(width: usize, height: usize, phase: usize) -> Frame {
    let empty = CFDictionary::<CFString, CFType>::empty();
    // SAFETY: constant key; an empty IOSurface properties dictionary requests
    // IOSurface backing, as ScreenCaptureKit buffers are.
    let attributes = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[&empty],
    );
    let mut raw = std::ptr::null_mut();
    // SAFETY: valid size/format/attributes and out-pointer.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Some(attributes.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    assert_eq!(status, 0);
    // SAFETY: +1 reference from the Create function.
    let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
    // SAFETY: lock/unlock around plane writes within the reported strides.
    unsafe {
        assert_eq!(
            CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0)),
            0
        );
        for (plane, rows) in [(0, height), (1, height / 2)] {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            for y in 0..rows {
                let row = std::slice::from_raw_parts_mut(base.add(y * stride), width);
                for (x, v) in row.iter_mut().enumerate() {
                    *v = if plane == 0 {
                        (16 + (x + y + phase * 8) % 220) as u8
                    } else {
                        128
                    };
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
    }
    Frame::new(buffer)
}

fn encode_and_decode(high: bool) {
    let (wake, woken) = std::sync::mpsc::channel::<()>();
    let wake = Mutex::new(wake);
    let quality = Quality {
        height: 720,
        fps: 30,
    };
    let mut encoder = encoder::VideoToolbox::new(
        (640, 360),
        quality,
        EncoderOptions {
            high_profile: high,
            quality_speed: Some(0),
        },
        Box::new(move || {
            let _ = wake.lock().unwrap().send(());
        }),
    )
    .expect("hardware VideoToolbox session");
    eprintln!(
        "{} / {} / speed {:?} / low latency {}",
        encoder.describe(),
        encoder.profile(),
        encoder.quality_vs_speed(),
        encoder.low_latency
    );
    let mut frames = Vec::new();
    for i in 0..12 {
        // Source larger than the session: VideoToolbox scales on the GPU.
        let pixels = Pixels {
            frame: surface(1280, 720, i),
            width: 1280,
            height: 720,
            arrived: Instant::now(),
        };
        let input = encoder.convert(&pixels).unwrap().unwrap();
        if i == 6 {
            encoder.force_keyframe().unwrap();
        }
        while !encoder.ready() {
            woken.recv_timeout(Duration::from_secs(1)).unwrap();
            frames.extend(encoder.poll().unwrap());
        }
        frames.extend(encoder.encode(&input, i as u64 * 33_333).unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while encoder.pending() > 0 && Instant::now() < deadline {
        let _ = woken.recv_timeout(Duration::from_millis(100));
        frames.extend(encoder.poll().unwrap());
    }
    assert!(frames.len() >= 10, "{} frames", frames.len());
    let keyframes = frames
        .iter()
        .filter(|f| crate::screen::recovery_frame(&f.data))
        .count();
    assert!(keyframes >= 2, "initial and forced IDR, got {keyframes}");
    assert!(crate::screen::recovery_frame(&frames[0].data));
    let mut decoder = openh264::decoder::Decoder::new().unwrap();
    let mut decoded = 0;
    for frame in &frames {
        assert!(thiscord_shared::h264::bounded_parameter_sets_with(
            &frame.data,
            high
        ));
        if let Some(image) = decoder.decode(&frame.data).unwrap() {
            use openh264::formats::YUVSource;
            assert_eq!(image.dimensions(), (640, 360));
            decoded += 1;
        }
    }
    assert!(decoded >= 10, "decoded {decoded}");
    let _ = high;
}

#[test]
fn videotoolbox_encodes_iosurface_nv12_with_forced_keyframes() {
    encode_and_decode(false);
}

#[test]
fn videotoolbox_high_profile_stays_within_receiver_bounds() {
    // openh264 decodes only baseline; validate High through the shared parser.
    let (wake, _woken) = std::sync::mpsc::channel::<()>();
    let wake = Mutex::new(wake);
    let mut encoder = encoder::VideoToolbox::new(
        (1280, 720),
        Quality {
            height: 720,
            fps: 60,
        },
        EncoderOptions {
            high_profile: true,
            quality_speed: None,
        },
        Box::new(move || {
            let _ = wake.lock().unwrap().send(());
        }),
    )
    .unwrap();
    assert_ne!(encoder.profile(), "baseline");
    let mut frames = Vec::new();
    for i in 0..4 {
        let input = surface(1280, 720, i);
        frames.extend(encoder.encode(&input, i as u64 * 16_667).unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while encoder.pending() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        frames.extend(encoder.poll().unwrap());
    }
    assert!(!frames.is_empty());
    assert!(crate::screen::recovery_frame(&frames[0].data));
    assert_eq!(
        thiscord_shared::h264::parameter_set_profile(&frames[0].data, true),
        Ok(Some(thiscord_shared::h264::Profile::High))
    );
}

/// End-to-end ScreenCaptureKit -> VideoToolbox -> paced queue on the main
/// display. Needs Screen Recording permission for the test process.
#[test]
#[ignore = "needs a logged-in display session and Screen Recording permission"]
fn capture_main_display_end_to_end() {
    let sources = sources().expect("sources");
    let display = sources
        .iter()
        .find(|s| matches!(s.id, SourceId::Monitor(_)))
        .expect("a display")
        .id
        .clone();
    let stop = Arc::new(AtomicBool::new(false));
    let demand = Arc::new(crate::screen::adaptation::Demand::default());
    demand.update(5_000_000);
    let metrics = Metrics::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let stopper = stop.clone();
    let drain = std::thread::spawn(move || {
        let mut frames = 0;
        let until = Instant::now() + Duration::from_secs(4);
        while Instant::now() < until {
            match rx.try_recv() {
                Ok(_) => frames += 1,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        stopper.store(true, Ordering::Release);
        frames
    });
    let started = Instant::now();
    let result = run_layers(
        &display,
        Quality {
            height: 1080,
            fps: 30,
        },
        stop,
        Connection::default(),
        started,
        tx,
        metrics.clone(),
        Arc::default(),
        demand,
        None,
        Arc::default(),
        |line| eprintln!("encoder: {line}"),
    );
    let frames = drain.join().unwrap();
    let snapshot = metrics.snapshot();
    eprintln!("result={result:?} queued_frames={frames}");
    eprintln!("labels={:?}", snapshot.labels);
    eprintln!("counters={:?}", snapshot.counters);
    result.unwrap();
    assert!(frames > 10, "queued {frames}");
}

#[test]
#[ignore = "needs Screen Recording permission and macOS 13"]
fn system_audio_runs_and_stops() {
    let stop = AtomicBool::new(false);
    let metrics = Metrics::default();
    let until = Instant::now() + Duration::from_secs(2);
    let mut samples = 0;
    let result = system_audio(&stop, &|| Instant::now() < until, &metrics, &mut |pcm| {
        samples += pcm.len();
        Ok(())
    });
    eprintln!(
        "result={result:?} samples={samples} counters={:?}",
        metrics.snapshot().counters
    );
    result.unwrap();
}

#[test]
fn window_points_follow_live_window_bounds() {
    use objc2_core_foundation::CFNumber;
    use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowNumber};
    assert_eq!(content::window_points(u32::MAX - 7), None);
    let Some(list) = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0) else {
        eprintln!("no window server session; skipping");
        return;
    };
    let ids: Vec<u32> = (0..list.count())
        .filter_map(|i| {
            // SAFETY: in-bounds index; elements are CFDictionaryRef owned by the list.
            let info = unsafe { list.value_at_index(i) }.cast::<CFDictionary>();
            // SAFETY: constant key; the value is a CFNumber or null.
            let key: *const CFString = unsafe { kCGWindowNumber };
            let number = unsafe { info.as_ref()?.value(key.cast()) }.cast::<CFNumber>();
            // SAFETY: owned by the list for this scope.
            u32::try_from(unsafe { number.as_ref()? }.as_i64()?).ok()
        })
        .collect();
    eprintln!("{} on-screen windows", ids.len());
    let sizes: Vec<_> = ids
        .iter()
        .filter_map(|id| content::window_points(*id))
        .collect();
    assert_eq!(sizes.len(), ids.len(), "every listed window reports bounds");
    assert!(sizes.iter().all(|(w, h)| *w >= 0.0 && *h >= 0.0));
}
