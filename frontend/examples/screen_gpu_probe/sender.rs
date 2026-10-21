//! Actual hardware -> production Sender -> Inbox -> decoder. Synthetic pixels,
//! no accounts/network/capture. Two independent encoders share one source texture.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_frontend::{
    audio::connection::Connection,
    screen::{
        self, Outgoing,
        adaptation::{Demand, QualityController},
        gpu,
        hardware::HardwareEncoder,
        keyframe::KeyframePolicy,
        metrics::Metrics,
        pacing::{SendBudget, transport_budget},
        receive::Inbox,
        send::{LocalPreview, Sender},
    },
};
use thiscord_shared::screen::{LayerTargets, Quality};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

pub fn run(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let scene = super::scene::Scene::new(device, 1920, 1080)?;
    scene.draw(context, 0)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            for rate in [1_250_000, 200_000] {
                let (primary, low) = tokio::join!(
                    layer(device, &scene, Quality::default(), rate, 0),
                    layer(
                        device,
                        &scene,
                        LayerTargets::low_quality(Quality::default()),
                        rate,
                        1
                    ),
                );
                primary?;
                low?;
            }
            Ok(())
        })
}

async fn layer(
    device: &ID3D11Device,
    scene: &super::scene::Scene,
    maximum: Quality,
    initial_rate: u32,
    layer: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let demand = Arc::new(Demand::default());
    let metrics = Metrics::default();
    let (preview, inbox) = Inbox::channel();
    let force = Arc::new(AtomicBool::new(false));
    let sender = Sender {
        local: Some(LocalPreview {
            sender: Arc::new(preview),
            epoch: Arc::new(AtomicU32::new(7)),
            visible: Arc::new(AtomicBool::new(true)),
        }),
        source_epoch: Some(Arc::new(AtomicU32::new(7))),
        connection: Connection::default(),
        video: screen::track(if layer == 0 { 901 } else { 904 }),
        demand: demand.clone(),
        network_demand: Some(Arc::new(Demand::default())),
        force_keyframe: force.clone(),
        sequence: Default::default(),
        metrics: metrics.clone(),
    };
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let job = tokio::spawn(sender.run(Arc::new(AtomicBool::new(false)), maximum, rx));
    let mut control = QualityController::new(maximum);
    let mut policy = KeyframePolicy::default();
    let mut budget = SendBudget::new(1);
    let mut previous = None;
    let mut codec: Option<(gpu::Processor, HardwareEncoder)> = None;
    let start = Instant::now();
    let decode_inbox = inbox.clone();
    // Inbox::next blocks. Keep assembly consumption/decoding on its own worker,
    // as production does, so one encoder cannot block the other's async sender.
    let decode_job = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let mut decoder = openh264::decoder::Decoder::new().map_err(|e| e.to_string())?;
        let mut first_delivery = None;
        let mut decoded = 0;
        while let Some(received) = decode_inbox.next() {
            if received.reset {
                decoder = openh264::decoder::Decoder::new().map_err(|e| e.to_string())?;
            }
            if decoder
                .decode(&received.data)
                .map_err(|e| e.to_string())?
                .is_none()
            {
                return Err("complete frame must decode".into());
            }
            decoded += 1;
            first_delivery.get_or_insert(start.elapsed());
        }
        Ok((decoded, first_delivery))
    });
    let mut inputs = 0;
    let mut changes = 0;
    let mut last_rejected = 0;
    // Twenty seconds reproduces the original endless cycle. The floor case
    // then gains capacity, verifying recovery can restore normal dimensions/FPS.
    let seconds = if initial_rate == 200_000 { 34 } else { 20 };
    while start.elapsed() < Duration::from_secs(seconds) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let rate = if start.elapsed() >= Duration::from_secs(20) {
            1_500_000
        } else {
            initial_rate
        };
        demand.update(rate);
        let quality = control
            .update_with_recovery(rate, demand.take_encoder_feedback(), Instant::now())
            .unwrap();
        let scale = control.scale_percent();
        let configuration = (quality, scale);
        if previous != Some(configuration) {
            let w = (quality.width() * scale / 100) & !1;
            let h = (quality.height * scale / 100) & !1;
            codec = Some((
                gpu::Processor::new(device, w, h)?,
                HardwareEncoder::new_gpu(w as usize, h as usize, quality, device)?,
            ));
            policy = KeyframePolicy::default();
            force.store(true, Ordering::Release);
            changes += 1;
            println!(
                "sender layer={layer} demand={rate} at_ms={} mode={quality:?} scale={scale} dimensions={w}x{h}",
                start.elapsed().as_millis()
            );
            previous = Some(configuration);
        }
        budget.set_bitrate(transport_budget(quality.bitrate(), rate));
        if !policy.refresh_due(Instant::now(), force.load(Ordering::Acquire))
            || !budget.has_capacity()
        {
            continue;
        }
        let (processor, encoder) = codec.as_mut().unwrap();
        let Some(surface) = processor.process(&scene.texture, 3.0, true)? else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !encoder.ready() {
            assert!(encoder.poll()?.is_empty());
            assert!(Instant::now() < deadline, "encoder input timeout");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let now = Instant::now();
        let requested = policy.feedback_ready(now) && force.swap(false, Ordering::AcqRel);
        let forced = requested || policy.periodic_due(now);
        if forced {
            encoder.force_keyframe()?;
        }
        let timestamp = now.duration_since(start).as_micros() as u64;
        let mut frames = encoder.encode_texture(surface, timestamp)?;
        policy.submitted(now, forced);
        while frames.is_empty() {
            frames.extend(encoder.poll()?);
            assert!(Instant::now() < deadline, "encoder output timeout");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        for frame in frames {
            let size = frame.data.len();
            let keyframe = screen::recovery_frame(&frame.data);
            if keyframe {
                policy.observed(Instant::now());
            }
            let packets =
                screen::packetize(frame.data, &mut 0, (timestamp * 90 / 1000) as u32).unwrap();
            let bytes = packets.iter().map(|p| p.payload.len() + 64).sum();
            tx.send(Outgoing {
                bitrate: quality.bitrate(),
                captured_at: now,
                enqueued_at: Instant::now(),
                packets,
                keyframe,
                budget: budget.track(bytes),
            })
            .await?;
            let deadline = Instant::now() + Duration::from_secs(1);
            while budget.bytes() != 0 {
                assert!(Instant::now() < deadline, "sender drain timeout");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let rejected = metrics.snapshot().counters["sender_burst_recovery"];
            if rejected != last_rejected || inputs < 3 {
                println!(
                    "sender layer={layer} demand={rate} at_ms={} idr={keyframe} bytes={size} rejected={rejected}",
                    start.elapsed().as_millis()
                );
            }
            inputs += 1;
            last_rejected = rejected;
        }
    }
    drop(tx);
    job.await?;
    let (decoded, first_delivery) = decode_job.await??;
    let first = first_delivery.expect("no decodable recovery");
    assert!(first < Duration::from_secs(if initial_rate == 200_000 { 3 } else { 2 }));
    assert!(decoded >= 18, "static heartbeat must continue after join");
    assert_eq!(inbox.metrics().snapshot().counters["expired_frames"], 0);
    assert_eq!(
        previous,
        Some((
            Quality {
                height: 720,
                fps: 15
            },
            100
        ))
    );
    if initial_rate == 1_250_000 {
        assert_eq!(changes, 1, "cold retry must preserve quality");
    }
    let assembly = inbox.metrics().snapshot().timings["assembly"].max_us;
    assert!(assembly < 250_000);
    println!(
        "PASS sender layer={layer} initial_demand={initial_rate} first_ms={} decoded={decoded} rejected={last_rejected} configurations={changes} assembly_max_us={assembly}",
        first.as_millis()
    );
    Ok(())
}
