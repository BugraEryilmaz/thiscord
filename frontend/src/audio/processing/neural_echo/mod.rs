//! Experimental REE v2 adapter at AEC3's internal residual-power boundary.
//! Feature/mask conventions follow WebRTC revision 526e228d25f83b1023760d3835f33d622c7b9f5f.
// Portions adapted from WebRTC, Copyright (c) 2025 The WebRTC project authors.
// BSD license and patent grant: vendor/WEBRTC_LICENSE.txt, WEBRTC_PATENTS.txt.
// Modified for Thiscord's mono Rust processing worker.
pub mod model;
use model::{BINS, Model};
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use sonora::{EchoCanceller3Config, NeuralResidualEstimator, NeuralResidualInput};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct NeuralEcho {
    model: Model,
    failed: Arc<AtomicBool>,
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    work: [Complex32; 256],
    window: [f32; 256],
    history: [[f32; 256]; 2],
    pending: usize,
    masks: [[f32; 65]; 2],
    ready: bool,
}
impl std::fmt::Debug for NeuralEcho {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralEcho")
            .field("ready", &self.ready)
            .finish_non_exhaustive()
    }
}
impl NeuralEcho {
    pub fn new(mut model: Model, failed: Arc<AtomicBool>) -> Self {
        model.reset();
        let fft = FftPlanner::new().plan_fft_forward(256);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        Self {
            model,
            failed,
            fft,
            scratch,
            work: [Complex32::default(); 256],
            window: std::array::from_fn(|i| {
                (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / 255.0).cos()).sqrt() / 32768.0
            }),
            history: [[0.0; 256]; 2],
            pending: 0,
            masks: [[0.0; 65]; 2],
            ready: false,
        }
    }
    fn features(&mut self, channel: usize) -> [f32; BINS] {
        for i in 0..256 {
            self.work[i] = Complex32::new(self.history[channel][i] * self.window[i], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.work, &mut self.scratch);
        std::array::from_fn(|i| self.work[i].norm_sqr().powf(0.15))
    }
    fn step(&mut self, cancelled: &[f32; 64], reference: &[f32]) -> Result<(), String> {
        if reference.len() != 64 || cancelled.iter().chain(reference).any(|v| !v.is_finite()) {
            return Err("Invalid neural echo input".into());
        }
        self.history[0][128 + self.pending..192 + self.pending].copy_from_slice(cancelled);
        self.history[1][128 + self.pending..192 + self.pending].copy_from_slice(reference);
        self.pending += 64;
        if self.pending == 128 {
            let a = self.features(0);
            let b = self.features(1);
            let (mask, unbounded) = self.model.infer(&a, &b)?;
            self.masks = [power_mask(&mask), power_mask(&unbounded)];
            for history in &mut self.history {
                history.copy_within(128..256, 0);
            }
            self.pending = 0;
            self.ready = true;
        }
        Ok(())
    }
}
/// Reduce 129 bins to AEC3's 65 using the maximum, then convert the model's
/// magnitude-complement output to an echo power fraction: 1 - (1 - m)^2.
fn power_mask(input: &[f32; BINS]) -> [f32; 65] {
    std::array::from_fn(|i| {
        let m = if i == 0 {
            input[0]
        } else {
            input[2 * i - 1].max(input[2 * i])
        };
        let m = m.clamp(0.0, 1.0);
        1.0 - (1.0 - m).powi(2)
    })
}
impl NeuralResidualEstimator for NeuralEcho {
    fn estimate(
        &mut self,
        input: NeuralResidualInput<'_>,
        residual: &mut [f32; 65],
        unbounded: &mut [f32; 65],
    ) -> bool {
        if self.failed.load(Ordering::Relaxed) {
            return false;
        }
        if self.step(input.linear, input.render).is_err() {
            self.failed.store(true, Ordering::Relaxed);
            return false;
        }
        if !self.ready {
            return false;
        }
        for i in 0..65 {
            unbounded[i] = input.linear_power[i] * self.masks[1][i];
            residual[i] = if input.dominant_nearend {
                unbounded[i]
            } else {
                input.linear_power[i] * self.masks[0][i]
            };
        }
        true
    }
    fn reset(&mut self) {
        self.model.reset();
        self.history.fill([0.0; 256]);
        self.masks.fill([0.0; 65]);
        self.pending = 0;
        self.ready = false;
        // An inference failure is latched until the application explicitly
        // restarts audio; an internal echo-path change must not hide it.
    }
    fn configure(&self, config: &mut EchoCanceller3Config) {
        let s = &mut config.suppressor;
        s.nearend_average_blocks = 1;
        for tuning in [&mut s.normal_tuning, &mut s.nearend_tuning] {
            for mask in [&mut tuning.mask_lf, &mut tuning.mask_hf] {
                mask.enr_transparent = 0.0;
                mask.enr_suppress = 1.0;
                mask.emr_transparent = 0.3;
            }
            tuning.max_inc_factor = 100.0;
            tuning.max_dec_factor_lf = 0.0;
        }
        s.dominant_nearend_detection.enr_threshold = 0.5;
        s.dominant_nearend_detection.trigger_threshold = 2;
        s.high_frequency_suppression.limiting_gain_band = 24;
        s.high_frequency_suppression.bands_in_limiting_gain = 3;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_conversion_selects_correct_pairs_and_dc() {
        let mut mask = [0.0; BINS];
        mask[0] = 0.5;
        mask[2] = 1.0;
        mask[127] = 0.25;
        let power = power_mask(&mask);
        assert_eq!(power[0], 0.75);
        assert_eq!(power[1], 1.0);
        assert_eq!(power[2], 0.0);
        assert_eq!(power[64], 0.4375);
    }
}
