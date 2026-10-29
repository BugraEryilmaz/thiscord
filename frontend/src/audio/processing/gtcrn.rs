//! The only module that knows GTCRN/tract-onnx types and model details.
//! GTCRN is a 16 kHz model: 48 kHz blocks are decimated, enhanced in 16 ms
//! STFT frames, then interpolated back. Output contains nothing above 8 kHz.
use super::stages::{Block, CaptureStage, dry_mix};
use crate::audio::device_rate::Converter;
use crate::audio::failure::Failure;
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::sync::Arc;
use thiscord_shared::audio::NoiseSuppressionStrength;
use tract_onnx::prelude::*;
use tract_onnx::tract_core::internal::ensure;

mod gru;
mod indexing;

const MODEL: &[u8] = include_bytes!("../../../models/gtcrn_simple.onnx");
const RATE: u32 = 16_000;
const FFT: usize = 512;
const HOP: usize = 256;
const BINS: usize = FFT / 2 + 1;
/// One 10 ms capture block at the model rate.
const BLOCK: usize = 160;
/// Queued silence so each 10 ms block can be filled although 16 ms frames
/// complete irregularly: the largest `n mod 256` for `n = 160k` is 224.
const PREROLL: usize = 224;
const QUEUE: usize = 1024;
/// Streaming caches in the model's input order after `mix`.
const CACHES: [&[usize]; 3] = [&[2, 1, 16, 16, 33], &[2, 3, 1, 1, 16], &[2, 1, 33, 16]];

type Plan = TypedSimplePlan<TypedModel>;

pub struct Gtcrn {
    plan: Arc<Plan>,
    state: TypedSimpleState<TypedModel, Arc<Plan>>,
    caches: TVec<TValue>,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    window: [f32; FFT],
    spectrum: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Analysis window: previous hop followed by the hop being filled.
    frame: [f32; FFT],
    filled: usize,
    overlap: [f32; FFT],
    queue: [f32; QUEUE],
    queue_start: usize,
    queue_len: usize,
    down: Converter,
    up: Converter,
    mix: f32,
}
impl Gtcrn {
    pub fn new(strength: NoiseSuppressionStrength) -> Result<Self, String> {
        // Model bytes are embedded and pinned (see models/gtcrn_simple.txt).
        // No runtime download, Python, external process or user-supplied path.
        let plan = Self::plan().map_err(|_| "Cannot initialize the bundled GTCRN model")?;
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(FFT);
        let inverse = planner.plan_fft_inverse(FFT);
        let scratch = vec![
            Complex32::default();
            forward
                .get_inplace_scratch_len()
                .max(inverse.get_inplace_scratch_len())
        ];
        let mut result = Self {
            state: SimpleState::new(plan.clone()).map_err(|e| e.to_string())?,
            plan,
            caches: TVec::new(),
            forward,
            inverse,
            // Periodic sqrt-Hann analysis/synthesis, as used in training.
            window: std::array::from_fn(|n| {
                (0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / FFT as f32).cos()).sqrt()
            }),
            spectrum: vec![Complex32::default(); FFT],
            scratch,
            frame: [0.0; FFT],
            filled: HOP,
            overlap: [0.0; FFT],
            queue: [0.0; QUEUE],
            queue_start: 0,
            queue_len: 0,
            down: Converter::new(48_000, RATE),
            up: Converter::new(RATE, 48_000),
            mix: dry_mix(strength),
        };
        result.reset();
        // Exercise the inference kernels before devices run, then restore fresh state.
        for _ in 0..4 {
            result
                .process(&mut [0.01; 480])
                .map_err(|_| "Cannot run the bundled GTCRN model")?;
        }
        result.reset();
        Ok(result)
    }
    fn plan() -> TractResult<Arc<Plan>> {
        let mut onnx = tract_onnx::onnx();
        gru::register(&mut onnx);
        Self::plan_with(&onnx, true)
    }
    /// `native` replaces tract evaluation paths that miss the 10 ms deadline;
    /// tests compare against the unmodified tract graph.
    fn plan_with(onnx: &tract_onnx::Onnx, native: bool) -> TractResult<Arc<Plan>> {
        let mut model = onnx.model_for_read(&mut std::io::Cursor::new(MODEL))?;
        model = model.with_input_fact(0, f32::fact([1, BINS, 1, 2]).into())?;
        for (index, shape) in CACHES.iter().enumerate() {
            model = model.with_input_fact(index + 1, f32::fact(*shape).into())?;
        }
        let mut model = model.into_optimized()?;
        if native {
            indexing::accelerate(&mut model);
        }
        Ok(Arc::new(model.into_runnable()?))
    }
    fn push(&mut self, sample: f32) -> TractResult<()> {
        self.frame[self.filled] = sample;
        self.filled += 1;
        if self.filled == FFT {
            self.enhance_frame()?;
            self.frame.copy_within(HOP.., 0);
            self.filled = HOP;
        }
        Ok(())
    }
    fn enhance_frame(&mut self) -> TractResult<()> {
        for ((bin, sample), window) in self.spectrum.iter_mut().zip(self.frame).zip(self.window) {
            *bin = Complex32::new(sample * window, 0.0);
        }
        self.forward
            .process_with_scratch(&mut self.spectrum, &mut self.scratch);
        // (batch, frequency, time, real/imaginary) = (1, 257, 1, 2)
        let mut mix = Tensor::zero::<f32>(&[1, BINS, 1, 2])?;
        for (pair, bin) in mix
            .as_slice_mut::<f32>()?
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.spectrum)
        {
            *pair = [bin.re, bin.im];
        }
        let mut inputs = tvec![mix.into_tvalue()];
        inputs.extend(self.caches.drain(..));
        let mut outputs = self.state.run(inputs)?;
        ensure!(outputs.len() == 4, "unexpected GTCRN outputs");
        let enhanced = outputs.remove(0);
        self.caches = outputs;
        let enhanced = enhanced.as_slice::<f32>()?;
        ensure!(enhanced.len() == BINS * 2, "unexpected GTCRN spectrum");
        // Limit attenuation by mixing back the unprocessed spectrum, then
        // restore the Hermitian half for a real inverse transform.
        for (bin, [re, im]) in enhanced.as_chunks::<2>().0.iter().enumerate() {
            let wet = Complex32::new(*re, *im);
            self.spectrum[bin] = wet * (1.0 - self.mix) + self.spectrum[bin] * self.mix;
        }
        self.spectrum[0].im = 0.0;
        self.spectrum[FFT / 2].im = 0.0;
        for bin in 1..FFT / 2 {
            self.spectrum[FFT - bin] = self.spectrum[bin].conj();
        }
        self.inverse
            .process_with_scratch(&mut self.spectrum, &mut self.scratch);
        // Squared sqrt-Hann windows at 50% overlap sum to one.
        for ((sum, bin), window) in self.overlap.iter_mut().zip(&self.spectrum).zip(self.window) {
            *sum += bin.re / FFT as f32 * window;
        }
        ensure!(self.queue_len + HOP <= QUEUE, "GTCRN output queue overflow");
        for sample in &self.overlap[..HOP] {
            self.queue[(self.queue_start + self.queue_len) % QUEUE] = *sample;
            self.queue_len += 1;
        }
        self.overlap.copy_within(HOP.., 0);
        self.overlap[HOP..].fill(0.0);
        Ok(())
    }
    fn pop(&mut self) -> f32 {
        // PREROLL guarantees a sample; a shortfall would be a framing bug.
        debug_assert!(self.queue_len > 0);
        if self.queue_len == 0 {
            return 0.0;
        }
        let sample = self.queue[self.queue_start];
        self.queue_start = (self.queue_start + 1) % QUEUE;
        self.queue_len -= 1;
        sample
    }
    fn failed() -> Failure {
        Failure::new(
            "gtcrn",
            "inference_failed",
            "Inference failed; audio stopped. Disable suppression and reconnect.",
        )
    }
}
impl CaptureStage for Gtcrn {
    fn name(&self) -> &'static str {
        "GTCRN"
    }
    fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        let mut decimated = 0;
        for &sample in block.iter() {
            self.down.push(sample);
            while let Some(sample) = self.down.pop() {
                decimated += 1;
                self.push(sample).map_err(|_| Self::failed())?;
            }
        }
        // The 3:1 integer phase clock yields exactly 160 samples per 10 ms.
        debug_assert_eq!(decimated, BLOCK);
        let mut output = block.iter_mut();
        for _ in 0..decimated {
            let sample = self.pop();
            self.up.push(sample);
            while let Some(sample) = self.up.pop() {
                if let Some(out) = output.next() {
                    *out = sample;
                }
            }
        }
        Ok(())
    }
    fn reset(&mut self) {
        // Caches are explicit model inputs; a new run state drops any session values.
        if let Ok(state) = SimpleState::new(self.plan.clone()) {
            self.state = state;
        }
        self.caches = CACHES
            .iter()
            .map(|shape| Tensor::zero::<f32>(shape).unwrap().into_tvalue())
            .collect();
        self.frame = [0.0; FFT];
        self.filled = HOP;
        self.overlap = [0.0; FFT];
        self.queue = [0.0; QUEUE];
        self.queue_start = 0;
        self.queue_len = PREROLL;
        // Recreate the converters so their integer phase starts block-aligned.
        self.down = Converter::new(48_000, RATE);
        self.up = Converter::new(RATE, 48_000);
    }
    fn delay_samples(&self) -> usize {
        // Hop history plus preroll at 16 kHz, then both resampler group delays.
        let framing = (HOP + PREROLL) * 3;
        let filters = (self.down.latency() + self.up.latency()).as_secs_f64() * 48_000.0;
        framing + filters.round() as usize
    }
    fn set_strength(&mut self, strength: NoiseSuppressionStrength) {
        self.mix = dry_mix(strength);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Native GRU/indexing must match tract's own lowering of the same ONNX graph,
    /// with the model's explicit caches carried across frames. tract's Scan keeps
    /// hidden state inside a reused `SimpleState` and ignores later initial-state
    /// inputs, so the reference runs each frame on a fresh state (ONNX semantics);
    /// the native plan runs on one reused state exactly as in production.
    #[test]
    fn native_evaluation_matches_tract_reference() {
        let native = Gtcrn::plan().unwrap();
        let reference = Gtcrn::plan_with(&tract_onnx::onnx(), false).unwrap();
        assert!(
            native
                .model()
                .nodes()
                .iter()
                .all(|node| !matches!(node.op().name().as_ref(), "Scan" | "Gather" | "ScatterNd"))
        );
        let caches = || -> TVec<TValue> {
            CACHES
                .iter()
                .map(|shape| Tensor::zero::<f32>(shape).unwrap().into_tvalue())
                .collect()
        };
        let (mut native_caches, mut reference_caches) = (caches(), caches());
        let mut native = SimpleState::new(native).unwrap();
        let mut seed = 9_u32;
        let mut worst = 0.0_f32;
        for frame in 0..60 {
            let spectrum: Vec<f32> = (0..BINS * 2)
                .map(|_| crate::audio::fixture::noise(&mut seed) * 40.0)
                .collect();
            let mix = || {
                Tensor::from_shape(&[1, BINS, 1, 2], &spectrum)
                    .unwrap()
                    .into_tvalue()
            };
            let mut inputs = tvec![mix()];
            inputs.extend(native_caches.drain(..));
            let mut a = native.run(inputs).unwrap();
            let mut inputs = tvec![mix()];
            inputs.extend(reference_caches.drain(..));
            let mut b = reference.run(inputs).unwrap();
            for (index, (a, b)) in a.iter().zip(&b).enumerate() {
                let (a, b) = (a.as_slice::<f32>().unwrap(), b.as_slice::<f32>().unwrap());
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b) {
                    let error = (a - b).abs() / b.abs().max(1.0);
                    worst = worst.max(error);
                    assert!(error < 1e-3, "frame {frame} output {index}: {a} vs {b}");
                }
            }
            native_caches = a.drain(1..).collect();
            reference_caches = b.drain(1..).collect();
        }
        eprintln!("GTCRN native evaluation worst relative error: {worst:e}");
    }
}
