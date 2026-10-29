//! The only module that knows DeepFilterNet/tract types and model details.
use super::stages::{Block, CaptureStage, attenuation_limit_db};
use crate::audio::failure::Failure;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};
use thiscord_shared::audio::NoiseSuppressionStrength;

/// libDF treats limits of 100 dB or more as unlimited attenuation.
fn limit_db(strength: NoiseSuppressionStrength) -> f32 {
    attenuation_limit_db(strength).unwrap_or(100.0)
}

pub struct DeepFilter {
    model: DfTract,
    pristine: DfTract,
    delay: usize,
}
impl DeepFilter {
    pub fn new(strength: NoiseSuppressionStrength) -> Result<Self, String> {
        // Model bytes are bundled by the pinned dependency. No runtime download,
        // Python, external process, device access or user-supplied model path.
        // A finite limit mixes back unprocessed spectrum, trading residual noise
        // for less speech distortion; upstream's default removes all it can.
        let runtime = RuntimeParams::default().with_atten_lim(limit_db(strength));
        let pristine = DfTract::new(DfParams::default(), &runtime)
            .map_err(|_| "Cannot initialize the bundled DeepFilterNet3 model")?;
        if (pristine.sr, pristine.hop_size, pristine.ch) != (48_000, 480, 1) {
            return Err("DeepFilterNet3 requires 48 kHz / 480-sample mono blocks".into());
        }
        let delay = pristine.fft_size - pristine.hop_size + pristine.lookahead * pristine.hop_size;
        let mut result = Self {
            model: pristine.clone(),
            pristine,
            delay,
        };
        // Exercise the inference kernels before devices run, then restore fresh
        // recurrent/FFT state. Silence alone can skip upstream inference.
        for _ in 0..3 {
            result.process(&mut [0.01; 480])?;
        }
        result.reset();
        Ok(result)
    }
}
impl CaptureStage for DeepFilter {
    fn name(&self) -> &'static str {
        "DeepFilterNet3"
    }
    fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        let mut out = [0.0; 480];
        self.model
            .process(
                ArrayView2::from_shape((1, 480), &block[..])
                    .map_err(|e| Failure::new("deep_filter", "array_shape", e.to_string()))?,
                ArrayViewMut2::from_shape((1, 480), &mut out[..])
                    .map_err(|e| Failure::new("deep_filter", "array_shape", e.to_string()))?,
            )
            .map_err(|_| {
                Failure::new(
                    "deep_filter",
                    "inference_failed",
                    "Inference failed; audio stopped. Disable suppression and reconnect.",
                )
            })?;
        *block = out;
        Ok(())
    }
    fn reset(&mut self) {
        // Upstream init() does not clear every rolling/recurrent state. Cloning
        // a pristine instance shares immutable plans but restores all history.
        self.model = self.pristine.clone();
    }
    fn delay_samples(&self) -> usize {
        self.delay
    }
    fn set_strength(&mut self, strength: NoiseSuppressionStrength) {
        // The limit is a mix factor, not model history; keep the live state.
        self.model.set_atten_lim(limit_db(strength));
        self.pristine.set_atten_lim(limit_db(strength));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{fixture, processing::denormals, processing::stages::CaptureChain};

    #[test]
    fn long_silence_stays_finite_then_speech_and_privacy_reset_work() {
        let _float_mode = denormals::Guard::new();
        let mut chain = CaptureChain::default();
        chain.push(
            true,
            DeepFilter::new(NoiseSuppressionStrength::Maximum).unwrap(),
        );

        // 100 seconds of real 10 ms inference calls, without resetting history.
        // With FTZ/DAZ the unfloored upstream normalization fails at block 7774.
        for frame in 1..=10_000 {
            let mut block = [0.0; 480];
            chain.process(&mut block).unwrap_or_else(|error| {
                assert_eq!(block, [0.0; 480], "failure must silence the output");
                panic!("silent block {frame}: {}", error.details());
            });
            assert_eq!(block, [0.0; 480], "noise added at silent block {frame}");
        }

        let speech = fixture::speech();
        let mut energy = 0.0;
        for input in speech[..480 * 100].as_chunks::<480>().0 {
            let mut block = *input;
            chain.process(&mut block).unwrap();
            assert!(block.iter().all(|sample| sample.is_finite()));
            energy += fixture::energy(&block);
        }
        assert!(energy > 1e-6, "speech stayed muted after long silence");

        chain.reset();
        let mut fresh = DeepFilter::new(NoiseSuppressionStrength::Maximum).unwrap();
        // A privacy reset must discard the preceding speech, FFT and recurrent
        // history. Compare both silence and subsequent speech against fresh state.
        for input in std::iter::repeat_n([0.0; 480], 10)
            .chain(speech[..480 * 40].as_chunks::<480>().0.iter().copied())
        {
            let mut actual = input;
            let mut expected = input;
            chain.process(&mut actual).unwrap();
            fresh.process(&mut expected).unwrap();
            assert_eq!(actual, expected, "privacy reset retained model history");
        }
    }

    #[test]
    fn upstream_unit_normalization_stays_finite_through_long_silence() {
        let _float_mode = denormals::Guard::new();
        let mut state = [df::UNIT_NORM_INIT[1]];
        let mut transposed_state = state;
        for frame in 1..=10_000 {
            let mut spectrum = [df::Complex32::new(0.0, 0.0)];
            let mut transposed = [0.0; 2];
            df::band_unit_norm_t(&spectrum, &mut transposed_state, 0.99, &mut transposed);
            df::band_unit_norm(&mut spectrum, &mut state, 0.99);
            assert!(
                spectrum[0].re.is_finite() && spectrum[0].im.is_finite(),
                "unit normalization failed at silent block {frame}, state={state:?}"
            );
            assert_eq!(spectrum[0], df::Complex32::new(0.0, 0.0));
            assert_eq!(transposed, [0.0; 2], "transposed block {frame}");
            assert_eq!(state, transposed_state);
            assert!(state[0] >= f32::MIN_POSITIVE);
        }
        assert_eq!(state, [f32::MIN_POSITIVE]);

        // The floor must not freeze adaptation when nonzero input resumes.
        let input = df::Complex32::new(0.3, -0.4);
        let mut spectrum = [input];
        let mut transposed = [input.re, input.im];
        df::band_unit_norm_t(&spectrum, &mut transposed_state, 0.99, &mut transposed);
        df::band_unit_norm(&mut spectrum, &mut state, 0.99);
        assert!(state[0] > f32::MIN_POSITIVE);
        assert_eq!(state, transposed_state);
        assert_eq!([spectrum[0].re, spectrum[0].im], transposed);
        assert!(transposed.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn normalization_does_not_hide_invalid_state() {
        // A numeric floor must not turn a poisoned state into apparently valid
        // audio (f32::max would discard NaN). Let the chain fail closed instead.
        for invalid in [f32::NAN, f32::NEG_INFINITY] {
            let mut state = [invalid];
            let mut transposed_state = state;
            let mut spectrum = [df::Complex32::new(1.0, 0.0)];
            let mut transposed = [1.0, 0.0];
            df::band_unit_norm_t(&spectrum, &mut transposed_state, 0.99, &mut transposed);
            df::band_unit_norm(&mut spectrum, &mut state, 0.99);
            for actual in [state[0], transposed_state[0]] {
                if invalid.is_nan() {
                    assert!(actual.is_nan());
                } else {
                    assert_eq!(actual, invalid);
                }
            }
            assert!(!spectrum[0].re.is_finite() || !spectrum[0].im.is_finite());
            assert!(transposed.iter().any(|sample| !sample.is_finite()));
        }
    }
}
