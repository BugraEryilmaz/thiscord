//! The only module that knows nnnoiseless (pure-Rust RNNoise) types.
use super::stages::{Block, CaptureStage, dry_mix};
use crate::audio::failure::Failure;
use nnnoiseless::DenoiseState;
use thiscord_shared::audio::NoiseSuppressionStrength;

/// RNNoise works on 16-bit-scaled samples rather than normalized PCM.
const SCALE: f32 = 32_768.0;

pub struct Rnnoise {
    state: Box<DenoiseState<'static>>,
    /// Unprocessed previous block, aligned with RNNoise's one-frame output delay.
    dry: Block,
    mix: f32,
}
impl Rnnoise {
    pub fn new(strength: NoiseSuppressionStrength) -> Self {
        // Weights are compiled into the crate; no model file or runtime download.
        const _: () = assert!(DenoiseState::FRAME_SIZE == 480);
        let mut result = Self {
            state: DenoiseState::new(),
            dry: [0.0; 480],
            mix: dry_mix(strength),
        };
        // nnnoiseless builds FFT plans lazily (thread-local) and shared tables on
        // first use; run one block on the worker before devices start.
        result
            .process(&mut [0.01; 480])
            .expect("RNNoise processing is infallible");
        result.reset();
        result
    }
}
impl CaptureStage for Rnnoise {
    fn name(&self) -> &'static str {
        "RNNoise"
    }
    fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        let input = block.map(|sample| sample * SCALE);
        let mut wet = [0.0; 480];
        self.state.process_frame(&mut wet, &input);
        let dry = std::mem::replace(&mut self.dry, *block);
        for ((sample, wet), dry) in block.iter_mut().zip(wet).zip(dry) {
            // RNNoise's internal DC high-pass makes this mix approximate below
            // about 20 Hz; speech frequencies stay aligned.
            *sample = wet / SCALE * (1.0 - self.mix) + dry * self.mix;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.state = DenoiseState::new();
        self.dry = [0.0; 480];
    }
    fn delay_samples(&self) -> usize {
        480
    }
    fn set_strength(&mut self, strength: NoiseSuppressionStrength) {
        self.mix = dry_mix(strength);
    }
}
