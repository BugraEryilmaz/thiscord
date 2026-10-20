//! The sole sender-clock -> local 48 kHz playout-rate conversion. This runs on
//! the worker, before mixing. The device-rate converter runs AFTER the mix,
//! independently of this sender-clock/occupancy correction loop.
use std::sync::OnceLock;

use super::mixer::FRAME;

const TAPS: usize = 32;
const PHASES: usize = 256;
pub const DELAY: usize = TAPS / 2;
pub const MAX_OUTPUT: usize = 966;
static FILTER: OnceLock<[[f32; TAPS]; PHASES + 1]> = OnceLock::new();

pub struct Rate {
    history: [f32; FRAME + TAPS],
    phase: f64,
}

impl Default for Rate {
    fn default() -> Self {
        FILTER.get_or_init(|| {
            std::array::from_fn(|phase| {
                let mut taps = std::array::from_fn(|tap| {
                    let x = tap as f64 + phase as f64 / PHASES as f64 - DELAY as f64;
                    let sinc = if x.abs() < 1e-12 {
                        0.94
                    } else {
                        (std::f64::consts::PI * 0.94 * x).sin() / (std::f64::consts::PI * x)
                    };
                    (sinc * (0.5 + 0.5 * (std::f64::consts::PI * x / DELAY as f64).cos())) as f32
                });
                let sum: f32 = taps.iter().sum();
                for tap in &mut taps {
                    *tap /= sum;
                }
                taps
            })
        });
        Self {
            history: [0.0; FRAME + TAPS],
            phase: 0.0,
        }
    }
}

impl Rate {
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.phase = 0.0;
    }

    /// Always the same 16-sample group delay, including at unity. Preserve phase
    /// and filter history across Opus packets; never periodically drop a frame.
    pub fn process(&mut self, input: &[f32], ppm: f64, output: &mut [f32; MAX_OUTPUT]) -> usize {
        assert!(input.len() <= FRAME);
        self.history[TAPS..TAPS + input.len()].copy_from_slice(input);
        let step = 1.0
            + if ppm.is_finite() {
                ppm.clamp(-5000.0, 5000.0)
            } else {
                0.0
            } / 1_000_000.0;
        let filter = FILTER.get().expect("constructed on worker");
        let mut count = 0;
        while self.phase < input.len() as f64 && count < MAX_OUTPUT {
            let index = self.phase.floor() as usize + TAPS;
            let fraction = self.phase.fract() * PHASES as f64;
            let phase = fraction.floor() as usize;
            let weight = fraction.fract() as f32;
            let mut sample = 0.0;
            for (tap, (&left, &right)) in filter[phase].iter().zip(&filter[phase + 1]).enumerate() {
                let coefficient = left + weight * (right - left);
                sample += self.history[index - tap] * coefficient;
            }
            output[count] = sample;
            count += 1;
            self.phase += step;
        }
        self.phase -= input.len() as f64;
        self.history.copy_within(input.len()..input.len() + TAPS, 0);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_media_holes_preserve_filter_history_and_phase() {
        for ppm in [-5000.0, 0.0, 5000.0] {
            let input: [f32; FRAME] = std::array::from_fn(|i| (i as f32 * 0.1).sin() * 0.1);
            let mut whole = Rate::default();
            let mut split = Rate::default();
            for _ in 0..10 {
                let mut expected = [0.0; MAX_OUTPUT];
                let n = whole.process(&input, ppm, &mut expected);
                let mut actual = Vec::new();
                for chunk in [&input[..137], &input[137..138], &input[138..]] {
                    let mut output = [0.0; MAX_OUTPUT];
                    let len = split.process(chunk, ppm, &mut output);
                    actual.extend_from_slice(&output[..len]);
                }
                assert_eq!(actual.len(), n);
                assert!(
                    actual
                        .iter()
                        .zip(&expected)
                        .all(|(a, b)| (a - b).abs() < 1e-6)
                );
            }
        }
    }

    #[test]
    fn fractional_rate_has_bounded_work_continuous_phase_and_measured_tone_error() {
        for ppm in [-5000.0, -400.0, 0.0, 400.0, 5000.0] {
            for hz in [100.0, 1000.0, 8000.0, 16000.0] {
                let mut rate = Rate::default();
                let mut total: usize = 0;
                let mut error = 0.0;
                let mut signal = 0.0;
                for block in 0..100 {
                    let input: [f32; FRAME] = std::array::from_fn(|i| {
                        (std::f64::consts::TAU * hz * (block * FRAME + i) as f64 / 48000.0).sin()
                            as f32
                            * 0.5
                    });
                    let mut output = [0.0; MAX_OUTPUT];
                    let count = rate.process(&input, ppm, &mut output);
                    assert!((955..=MAX_OUTPUT).contains(&count));
                    for &sample in &output[..count] {
                        let t = total as f64 * (1.0 + ppm / 1e6) - DELAY as f64;
                        let expected = (std::f64::consts::TAU * hz * t / 48000.0).sin() * 0.5;
                        if total > 64 {
                            error += (f64::from(sample) - expected).powi(2);
                            signal += expected * expected;
                        }
                        total += 1;
                    }
                }
                let snr = 10.0 * (signal / error).log10();
                println!("sinc ppm={ppm} hz={hz} SNR={snr:.2} dB");
                assert!(snr > 45.0, "resampling distortion: {snr}");
                let expected = (100.0 * FRAME as f64 / (1.0 + ppm / 1e6)).ceil() as usize;
                assert!(total.abs_diff(expected) <= 1);
            }
        }
    }

    #[test]
    fn reset_removes_old_source_and_invalid_rates_are_bounded() {
        let mut rate = Rate::default();
        let mut output = [0.0; MAX_OUTPUT];
        rate.process(&[0.5; FRAME], 5000.0, &mut output);
        rate.reset();
        for ppm in [f64::NAN, f64::INFINITY, -1e12] {
            let n = rate.process(&[0.0; FRAME], ppm, &mut output);
            assert!(n <= MAX_OUTPUT);
            assert!(output[..n].iter().all(|v| *v == 0.0));
        }
    }
}
