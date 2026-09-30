//! Deterministic inputs shared by DSP regressions and the offline benchmark.
use std::io::Cursor;

pub fn speech() -> Vec<f32> {
    let mut wav =
        hound::WavReader::new(Cursor::new(include_bytes!("../fixtures/speech.wav"))).unwrap();
    assert_eq!(wav.spec().sample_rate, 48_000);
    assert_eq!(wav.spec().channels, 1);
    wav.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}
pub fn noise(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    (*state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32 * 0.035
}
pub fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|&s| f64::from(s).powi(2)).sum()
}
/// Aligned conventional SNR, penalizing gain/distortion rather than hiding it.
pub fn snr(clean: &[f32], output: &[f32]) -> f64 {
    let error: f64 = clean
        .iter()
        .zip(output)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum();
    10.0 * (energy(clean) / error.max(1e-20)).log10()
}

/// Find sample alignment for quality comparisons only (outside timing regions).
/// This includes filter-bank delay in Sonora/AEC, not just neural lookahead.
pub fn best_delay(clean: &[f32], output: &[f32], max_delay: usize) -> usize {
    (0..=max_delay)
        .max_by(|&a, &b| {
            let score = |lag: usize| {
                let mut dot = 0.0_f64;
                let mut power = 0.0_f64;
                for n in (4800..clean.len()).step_by(16) {
                    let y = f64::from(output[n + lag]);
                    dot += f64::from(clean[n]) * y;
                    power += y * y;
                }
                dot / power.max(1e-20).sqrt()
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap_or(0)
}
