use super::*;
use sonora::{EchoCanceller3Config, NeuralResidualEstimator, NeuralResidualInput};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Probe(Arc<AtomicUsize>);
impl NeuralResidualEstimator for Probe {
    fn estimate(
        &mut self,
        input: NeuralResidualInput<'_>,
        r: &mut [f32; 65],
        u: &mut [f32; 65],
    ) -> bool {
        assert_eq!(input.render.len(), 64);
        assert_eq!(input.capture.len(), 64);
        assert!(
            input
                .linear_power
                .iter()
                .all(|x| x.is_finite() && *x >= 0.0)
        );
        self.0.fetch_add(1, Ordering::Relaxed);
        r.fill(0.0);
        u.fill(0.0);
        true
    }
    fn reset(&mut self) {}
    fn configure(&self, _: &mut EchoCanceller3Config) {}
}
#[test]
fn internal_estimator_receives_aligned_blocks_only_with_aec_enabled() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut inactive = Processing::new(&AudioSettings::default()).unwrap();
    assert!(
        !inactive
            .apm
            .set_neural_estimator(Box::new(Probe(calls.clone())))
    );
    let mut active = Processing::new(&AudioSettings {
        echo_cancellation: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        active
            .apm
            .set_neural_estimator(Box::new(Probe(calls.clone())))
    );
    for i in 0..100 {
        let reference = std::array::from_fn(|j| ((i * 480 + j) as f32 * 0.017).sin() * 0.1);
        active.render(&reference).unwrap();
        active.capture_frame(&mut [0.01; 480], 40).unwrap();
    }
    assert!(calls.load(Ordering::Relaxed) > 200);
}
#[test]
fn model_failure_silences_capture_and_stays_latched_across_gap_reset() {
    let mut p = Processing::new(&AudioSettings::default()).unwrap();
    p.neural_failed.store(true, Ordering::Relaxed);
    for _ in 0..2 {
        let mut input = [0.2; 480];
        assert!(p.capture_frame(&mut input, 0).is_err());
        assert_eq!(input, [0.0; 480]);
        p.reset();
    }
}
#[test]
fn rejects_mode_changes_while_running_and_missing_models() {
    let mut p = Processing::new(&AudioSettings::default()).unwrap();
    let mut s = AudioSettings {
        neural_echo: true,
        ..Default::default()
    };
    assert!(p.settings(&s).is_err());
    assert!(Processing::new(&s).is_ok());
    s.neural_echo_model = Some("missing-neural-echo-model-do-not-create.tflite".into());
    assert!(Processing::new(&s).is_err());
}

#[test]
fn pinned_model_reset_bypass_and_composition() {
    let mut model = neural_echo::model::Model::bundled().unwrap();
    let first = model.infer(&[0.3; 129], &[0.5; 129]).unwrap();
    for _ in 0..10 {
        model.infer(&[0.7; 129], &[0.1; 129]).unwrap();
    }
    model.reset();
    assert_eq!(first, model.infer(&[0.3; 129], &[0.5; 129]).unwrap());
    assert!(model.infer(&[f32::NAN; 129], &[0.0; 129]).is_err());
    let mut s = AudioSettings {
        echo_cancellation: true,
        neural_echo: true,
        ..Default::default()
    };
    let mut p = Processing::new(&s).unwrap();
    for _ in 0..100 {
        p.render(&[0.1; 480]).unwrap();
        p.capture_frame(&mut [0.01; 480], 40).unwrap();
    }
    p.reset();
    s.echo_cancellation = false;
    p.settings(&s).unwrap();
    let mut frame = [0.02; 480];
    p.capture_frame(&mut frame, 0).unwrap();
    assert_eq!(frame, [0.02; 480]);
    s.echo_cancellation = true;
    s.noise_suppression = true;
    s.automatic_gain = true;
    p.settings(&s).unwrap();
    for _ in 0..100 {
        p.render(&[0.1; 480]).unwrap();
        p.capture_frame(&mut [0.01; 480], 40).unwrap();
    }
    assert!(!p.neural_failed.load(Ordering::Relaxed));
}

#[test]
fn pinned_model_echo_and_double_talk_regression() {
    let speech = fixture::speech();
    let count = 48_000 * 16;
    let render: Vec<_> = (0..count)
        .map(|i| speech[i % speech.len()] * 0.65)
        .collect();
    let local: Vec<_> = (0..count)
        .map(|i| {
            if (48_000 * 8..48_000 * 14).contains(&i) {
                speech[(i * 11 / 10 + 73123) % speech.len()] * 0.45
            } else {
                0.0
            }
        })
        .collect();
    for clipped in [false, true] {
        let speaker = |i: usize| {
            if clipped {
                render[i].clamp(-0.1, 0.1)
            } else {
                render[i]
            }
        };
        let echo: Vec<_> = (0..count)
            .map(|i| {
                i.checked_sub(1920).map_or(0.0, |j| speaker(j) * 0.6)
                    + i.checked_sub(2640).map_or(0.0, |j| speaker(j) * 0.15)
            })
            .collect();
        let mut retained = [0.0; 2];
        for (index, neural) in [false, true].into_iter().enumerate() {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                neural_echo: neural,
                ..Default::default()
            })
            .unwrap();
            let mut output = Vec::with_capacity(count);
            for offset in (0..count).step_by(480) {
                p.render(render[offset..offset + 480].try_into().unwrap())
                    .unwrap();
                let mut frame = std::array::from_fn(|i| local[offset + i] + echo[offset + i]);
                p.capture_frame(&mut frame, 40).unwrap();
                assert!(frame.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                output.extend_from_slice(&frame);
            }
            // Compare energy as well as correlation: a decorrelating/distorting
            // processor must not pass merely by producing unrelated output.
            let range = 48_000 * 4..48_000 * 8;
            let reduction = 10.0
                * (fixture::energy(&echo[range.clone()])
                    / fixture::energy(&output[range]).max(1e-20))
                .log10();
            retained[index] = fixture::correlated_gain(&local, &output, 48_000 * 9, 48_000 * 14);
            eprintln!(
                "neural={neural} clipped={clipped} echo energy reduction={reduction:.2} dB local gain={}",
                retained[index]
            );
            assert!(reduction > 25.0, "far-end echo leaked: {reduction}");
            assert!(retained[index] > 0.2, "local speech was suppressed");
        }
        assert!(
            retained[1] >= retained[0] * 0.8,
            "neural mode damaged double-talk speech"
        );
    }
}
