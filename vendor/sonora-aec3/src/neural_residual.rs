//! Thiscord extension point. Mono 16 kHz split-band signals use APM's +/-32768
//! scale. Called on the processing worker, never from an audio device callback.
use crate::config::EchoCanceller3Config;

/// Inputs at the internal AEC3 residual-estimation boundary.
#[derive(Debug)]
pub struct NeuralResidualInput<'a> {
    pub render: &'a [f32],
    pub capture: &'a [f32],
    pub linear: &'a [f32; 64],
    pub linear_power: &'a [f32; 65],
    pub dominant_nearend: bool,
}

/// Optional trained estimator. Returning false retains the conventional
/// estimate for this block; the application must surface persistent failures.
pub trait NeuralResidualEstimator: std::fmt::Debug + Send {
    fn estimate(
        &mut self,
        input: NeuralResidualInput<'_>,
        residual: &mut [f32; 65],
        unbounded: &mut [f32; 65],
    ) -> bool;
    fn reset(&mut self);
    fn configure(&self, config: &mut EchoCanceller3Config);
}
