use super::*;

fn settings() -> AudioSettings {
    AudioSettings {
        noise_suppression: true,
        noise_suppression_model: NoiseSuppressionModel::DeepFilterNet3,
        ..Default::default()
    }
}
fn run(p: &mut Processing, input: &[f32]) -> Vec<f32> {
    let mut output = Vec::with_capacity(input.len());
    for block in input.as_chunks::<480>().0 {
        let mut block = *block;
        p.capture_frame(&mut block, 0).unwrap();
        assert!(block.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        output.extend_from_slice(&block);
    }
    output
}

#[test]
fn deepfilter_noise_reduction_and_aligned_speech_preservation() {
    let mut p = Processing::new(&settings()).unwrap();
    let mut seed = 42;
    let noise: Vec<_> = (0..48_000 * 3).map(|_| fixture::noise(&mut seed)).collect();
    let output = run(&mut p, &noise);
    let reduction = 10.0
        * (fixture::energy(&noise[48_000..]) / fixture::energy(&output[48_000..]).max(1e-20))
            .log10();
    eprintln!("DFN3 stationary noise reduction: {reduction:.2} dB");
    assert!(reduction > 12.0, "noise reduction {reduction}");

    p.reset();
    let clean: Vec<_> = fixture::speech()
        .into_iter()
        .take(48_000 * 4)
        .map(|s| s * 0.7)
        .collect();
    let mut noisy: Vec<_> = clean
        .iter()
        .map(|s| s + fixture::noise(&mut seed))
        .collect();
    let input_snr = fixture::snr(&clean, &noisy);
    let delay = p.enhancement_delay_samples();
    assert!((480..=1920).contains(&delay));
    noisy.resize(clean.len() + delay, 0.0);
    let output = run(&mut p, &noisy);
    let aligned = &output[delay..delay + clean.len()];
    assert_eq!(fixture::best_delay(&clean, &output, delay), delay);
    let output_snr = fixture::snr(&clean, aligned);
    let retention = fixture::energy(aligned) / fixture::energy(&clean);
    eprintln!(
        "DFN3 synthetic speech: input {input_snr:.2} dB, output {output_snr:.2} dB; energy ratio {retention:.3}; delay {delay} samples"
    );
    // Muting the entire input would pass noise-only attenuation; it fails here.
    assert!((0.2..2.0).contains(&retention), "speech energy {retention}");
    assert!(
        output_snr > input_snr + 1.0,
        "speech regression: {input_snr} -> {output_snr}"
    );
}

#[test]
fn deepfilter_reset_bypass_reenable_and_channel_isolation() {
    let mut s = settings();
    let mut p = Processing::new(&s).unwrap();
    let speech = fixture::speech();
    let first = run(&mut p, &speech[..480 * 40]);
    for _ in 0..8 {
        p.reset();
        let again = run(&mut p, &speech[..480 * 40]);
        assert!(
            first.iter().zip(again).all(|(a, b)| (a - b).abs() < 1e-6),
            "reset retained model history"
        );
    }
    s.noise_suppression = false;
    p.settings(&s).unwrap();
    assert_eq!(p.enhancement_delay_samples(), 0);
    assert_eq!(run(&mut p, &speech[..480 * 10]), speech[..480 * 10]);
    s.noise_suppression = true;
    p.settings(&s).unwrap();
    assert_eq!(run(&mut p, &speech[..480 * 40]), first);
    p.reset();
    assert!(
        run(&mut p, &[0.0; 480 * 10]).iter().all(|x| x.abs() < 1e-8),
        "old stream leaked into new silence"
    );
    // Reject an unsupported hot model swap before touching the active chain.
    let bad = AudioSettings {
        noise_suppression_model: NoiseSuppressionModel::Sonora,
        ..s.clone()
    };
    assert!(p.settings(&bad).is_err());
    assert!(p.enhancement_delay_samples() > 0);
    // AEC and AGC remain independently composable with the neural stage.
    s.automatic_gain = true;
    s.echo_cancellation = true;
    p.settings(&s).unwrap();
    for block in speech[..480 * 30].as_chunks::<480>().0 {
        p.render(&[0.03; 480]).unwrap();
        let mut block = *block;
        p.capture_frame(&mut block, 40).unwrap();
        assert!(block.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
    }
    assert!(p.echo_diagnostics().unwrap().automatic_gain);
}

#[test]
fn deepfilter_queue_gap_discards_delayed_history_and_restarts() {
    let mut p = Processing::new(&settings()).unwrap();
    let (mut writer, mut reader) = frames::queue();
    for _ in 0..(frames::BLOCKS + 2) * 480 {
        writer.push(0.05);
    }
    while p.capture_queued(&mut reader).unwrap().is_some() {}
    for _ in 0..960 {
        writer.push(0.0);
    }
    let silence = p.capture_queued(&mut reader).unwrap().unwrap();
    assert_eq!(p.resets, 1);
    assert!(silence.iter().all(|x| x.abs() < 1e-8));
    let mut invalid = [0.01; 480];
    invalid[17] = f32::NAN;
    assert!(p.capture_frame(&mut invalid, 0).is_err());
    assert_eq!(invalid, [0.0; 480]);
}
