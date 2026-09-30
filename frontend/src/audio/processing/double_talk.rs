//! Deterministic speech-like probes; these do not replace real acoustic tests.
use super::*;

const RATE: usize = 48_000;

fn voice(n: usize, pitch: f64, offset: f64) -> f32 {
    let t = n as f64 / RATE as f64 + offset;
    // Independently modulated voiced harmonics and syllable envelopes. Unlike
    // stationary white-noise probes, the two sources overlap spectrally.
    let phase = std::f64::consts::TAU * pitch * t + 1.4 * (t * 7.0).sin();
    let envelope = (t * std::f64::consts::TAU * 3.7).sin().max(0.0);
    (envelope
        * (1..=12)
            .map(|h| (phase * h as f64).sin() / h as f64)
            .sum::<f64>()) as f32
}

/// Estimate the amplitude of a known source remaining in the output, allowing
/// up to 10 ms processing delay. This measures correlated echo, not all error.
fn correlated_gain(source: &[f32], output: &[f32], start: usize, end: usize) -> f64 {
    (0..=480)
        .step_by(8)
        .map(|lag| {
            let (mut dot, mut power) = (0.0, 0.0);
            for n in (start..end).step_by(8) {
                let x = f64::from(source[n - lag]);
                dot += x * f64::from(output[n]);
                power += x * x;
            }
            dot.abs() / power.max(1e-12)
        })
        .fold(0.0_f64, f64::max)
}

#[test]
fn double_talk_preserves_local_voice_and_reduces_correlated_echo() {
    let count = RATE * 16;
    let render: Vec<_> = (0..count).map(|n| voice(n, 173.0, 0.0) * 0.12).collect();
    let local: Vec<_> = (0..count)
        .map(|n| {
            if (RATE * 6..RATE * 14).contains(&n) {
                voice(n, 227.0, 0.073) * 0.025
            } else {
                0.0
            }
        })
        .collect();
    for distorted in [false, true] {
        let speaker = |n: usize| {
            let sample = render[n];
            if distorted {
                sample.clamp(-0.08, 0.08)
            } else {
                sample
            }
        };
        let echo: Vec<_> = (0..count)
            .map(|n| {
                n.checked_sub(1920).map_or(0.0, |j| speaker(j) * 0.6)
                    + n.checked_sub(2640).map_or(0.0, |j| speaker(j) * 0.15)
            })
            .collect();
        let bypass: Vec<_> = local.iter().zip(&echo).map(|(a, b)| a + b).collect();
        assert!(correlated_gain(&echo, &bypass, RATE * 8, RATE * 14) > 0.9);
        for automatic_gain in [false, true] {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                noise_suppression: true,
                automatic_gain,
                ..Default::default()
            })
            .unwrap();
            let mut control = Processing::new(&AudioSettings {
                noise_suppression: true,
                ..Default::default()
            })
            .unwrap();
            let mut output = Vec::with_capacity(count);
            let mut clean_output = Vec::with_capacity(count);
            for offset in (0..count).step_by(960) {
                for frame in render[offset..offset + 960].as_chunks::<480>().0 {
                    p.render(frame).unwrap();
                }
                let mut pcm = std::array::from_fn(|i| local[offset + i] + echo[offset + i]);
                p.capture(&mut pcm, 40).unwrap();
                assert!(pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                output.extend_from_slice(&pcm);
                let mut clean = std::array::from_fn(|i| local[offset + i]);
                control.capture(&mut clean, 0).unwrap();
                clean_output.extend_from_slice(&clean);
            }
            let echo_gain = correlated_gain(&echo, &output, RATE * 8, RATE * 14);
            let voice_gain = correlated_gain(&local, &output, RATE * 8, RATE * 14);
            let clean_gain = correlated_gain(&local, &clean_output, RATE * 8, RATE * 14);
            assert!(
                clean_gain > 0.5,
                "clean control must retain the local voice"
            );
            let reduction_db = -20.0 * echo_gain.max(1e-12).log10();
            eprintln!(
                "double talk: distortion={distorted}, AGC={automatic_gain}, correlated echo reduction={reduction_db:.1} dB, local gain={voice_gain:.2}, clean gain={clean_gain:.2}"
            );
            // Compare with noise-suppressed local speech at unity gain. AGC is
            // adaptive, so its unrelated gain on the clean control isn't a
            // valid minimum amplitude for this mixture.
            assert!(
                voice_gain > clean_gain * 0.5,
                "local speech suppressed: {voice_gain} vs {clean_gain}"
            );
            assert!(
                reduction_db > 12.0,
                "remaining correlated echo: {reduction_db:.1} dB"
            );
        }
    }
}
