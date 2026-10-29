use super::*;

fn settings(
    model: NoiseSuppressionModel,
    noise_suppression_strength: NoiseSuppressionStrength,
) -> AudioSettings {
    AudioSettings {
        noise_suppression: true,
        noise_suppression_model: model,
        noise_suppression_strength,
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
/// Low-pass (brown-like) noise, closer to fans/rumble than flat white noise.
/// RNNoise barely attenuates synthetic full-band white noise, treating it as
/// speech-like hiss; that is model behavior rather than a strength failure.
fn colored_noise(seed: &mut u32, samples: usize) -> Vec<f32> {
    let mut state = 0.0;
    (0..samples)
        .map(|_| {
            state = 0.98 * state + fixture::noise(seed);
            state * 0.2
        })
        .collect()
}
fn noise_reduction_db(p: &mut Processing) -> f64 {
    let mut seed = 42;
    let noise = colored_noise(&mut seed, 48_000 * 3);
    let output = run(p, &noise);
    10.0 * (fixture::energy(&noise[48_000..]) / fixture::energy(&output[48_000..]).max(1e-20))
        .log10()
}
fn new_models() -> Vec<NoiseSuppressionModel> {
    [NoiseSuppressionModel::Rnnoise, NoiseSuppressionModel::Gtcrn]
        .into_iter()
        .filter(|&model| available(model))
        .collect()
}
const STRENGTHS: [NoiseSuppressionStrength; 4] = [
    NoiseSuppressionStrength::Light,
    NoiseSuppressionStrength::Balanced,
    NoiseSuppressionStrength::Strong,
    NoiseSuppressionStrength::Maximum,
];

#[test]
fn new_models_reduce_noise_and_preserve_aligned_speech() {
    for model in new_models() {
        let mut p = Processing::new(&settings(model, NoiseSuppressionStrength::Maximum)).unwrap();
        let reduction = noise_reduction_db(&mut p);
        eprintln!("{model:?} stationary noise reduction: {reduction:.2} dB");
        assert!(reduction > 10.0, "{model:?} noise reduction {reduction}");

        p.reset();
        let mut seed = 7;
        let clean: Vec<_> = fixture::speech()
            .into_iter()
            .take(48_000 * 4)
            .map(|s| s * 0.7)
            .collect();
        let mut noisy: Vec<_> = clean
            .iter()
            .zip(colored_noise(&mut seed, clean.len()))
            .map(|(s, n)| s + n)
            .collect();
        let input_snr = fixture::snr(&clean, &noisy);
        let delay = p.enhancement_delay_samples();
        assert!((480..=1920).contains(&delay), "{model:?} delay {delay}");
        noisy.resize((clean.len() + delay + 48).next_multiple_of(480), 0.0);
        let output = run(&mut p, &noisy);
        let measured = fixture::best_delay(&clean, &output, delay + 48);
        assert!(
            measured.abs_diff(delay) <= 2,
            "{model:?} reported {delay}, measured {measured}"
        );
        let aligned = &output[measured..measured + clean.len()];
        let output_snr = fixture::snr(&clean, aligned);
        let retention = fixture::energy(aligned) / fixture::energy(&clean);
        eprintln!(
            "{model:?} synthetic speech: input {input_snr:.2} dB, output {output_snr:.2} dB; energy ratio {retention:.3}; delay {delay} samples"
        );
        // Muting the entire input would pass noise-only attenuation; it fails here.
        assert!(
            (0.2..2.0).contains(&retention),
            "{model:?} speech {retention}"
        );
        // Waveform SNR also penalizes coloration of the speech itself. RNNoise's
        // coarse band gains measured 10.50 -> 10.30 dB here despite ~50 dB noise
        // removal, so it only has to avoid a clear regression; GTCRN must improve.
        let required = match model {
            NoiseSuppressionModel::Rnnoise => -1.0,
            _ => 1.0,
        };
        assert!(
            output_snr > input_snr + required,
            "{model:?} speech regression: {input_snr} -> {output_snr}"
        );
    }
}

#[test]
fn strength_orders_noise_attenuation_for_every_model() {
    for model in MODELS.into_iter().filter(|&model| available(model)) {
        let reductions: Vec<f64> = STRENGTHS
            .iter()
            .map(|&strength| {
                noise_reduction_db(&mut Processing::new(&settings(model, strength)).unwrap())
            })
            .collect();
        eprintln!("{model:?} noise reduction by strength: {reductions:.2?} dB");
        for pair in reductions.windows(2) {
            assert!(
                pair[0] < pair[1] + 0.25,
                "{model:?} not ordered: {reductions:?}"
            );
        }
        assert!(
            reductions[3] > reductions[0] + 4.0,
            "{model:?} strength has no effect: {reductions:?}"
        );
        if model != NoiseSuppressionModel::Sonora {
            // Mixing back unprocessed signal caps attenuation near each limit.
            for (reduction, limit) in reductions.iter().zip([6.0, 12.0, 18.0]) {
                assert!(
                    *reduction < limit + 1.0,
                    "{model:?} exceeded {limit} dB: {reductions:?}"
                );
            }
            assert!(
                reductions[0] > 3.0,
                "{model:?} light did nothing: {reductions:?}"
            );
        }
    }
}

#[test]
fn strength_applies_live_without_model_replacement_or_lost_privacy_reset() {
    let speech = fixture::speech();
    for model in MODELS.into_iter().filter(|&model| available(model)) {
        let mut s = settings(model, NoiseSuppressionStrength::Maximum);
        let mut p = Processing::new(&s).unwrap();
        let maximum = noise_reduction_db(&mut p);
        s.noise_suppression_strength = NoiseSuppressionStrength::Light;
        assert!(!p.needs_replacement(&s), "{model:?}");
        p.settings(&s).unwrap();
        p.reset();
        let light = noise_reduction_db(&mut p);
        assert!(
            light + 3.0 < maximum,
            "{model:?}: light {light}, maximum {maximum}"
        );

        // Reset restores fresh state at the selected strength.
        let mut fresh = Processing::new(&s).unwrap();
        p.reset();
        let input = &speech[..480 * 40];
        assert_eq!(run(&mut p, input), run(&mut fresh, input), "{model:?}");
        // Bypass stays bit-exact regardless of strength.
        s.noise_suppression = false;
        p.settings(&s).unwrap();
        assert_eq!(p.enhancement_delay_samples(), 0);
        assert_eq!(run(&mut p, &speech[..480 * 10]), speech[..480 * 10]);
    }
}

#[test]
fn prewarmed_models_switch_live_and_start_fresh_at_current_strength() {
    let speech = &fixture::speech()[..480 * 40];
    let strength = NoiseSuppressionStrength::Balanced;
    let mut p = Processing::new(&settings(NoiseSuppressionModel::Sonora, strength)).unwrap();
    p.prewarm().unwrap();
    for model in MODELS
        .into_iter()
        .chain([NoiseSuppressionModel::Sonora])
        .filter(|&model| available(model))
    {
        let s = settings(model, strength);
        let mut fresh = Processing::new(&s).unwrap();
        run(&mut p, speech);
        assert_eq!(p.needs_replacement(&s), model != p.model);
        p.install(Prepared::new(&s).unwrap()).unwrap();
        assert_eq!(p.model, model);
        assert_eq!(
            p.enhancement_delay_samples(),
            fresh.enhancement_delay_samples()
        );
        // Delayed audio from the previous model must not leak into the new one.
        assert_eq!(run(&mut p, speech), run(&mut fresh, speech), "{model:?}");
    }
}

#[test]
fn new_models_stay_silent_through_long_silence_and_reject_invalid_input() {
    let _float_mode = denormals::Guard::new();
    for model in new_models() {
        let mut p = Processing::new(&settings(model, NoiseSuppressionStrength::Maximum)).unwrap();
        // 30 seconds of silence must neither add noise nor poison model state.
        let output = run(&mut p, &vec![0.0; 48_000 * 30]);
        assert!(output.iter().all(|x| x.abs() < 1e-6), "{model:?}");
        let speech = run(&mut p, &fixture::speech()[..480 * 100]);
        assert!(
            fixture::energy(&speech) > 1e-6,
            "{model:?} muted after silence"
        );
        let mut invalid = [0.01; 480];
        invalid[17] = f32::NAN;
        assert!(p.capture_frame(&mut invalid, 0).is_err());
        assert_eq!(invalid, [0.0; 480]);
    }
}
