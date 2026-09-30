//! The only module that knows DeepFilterNet/tract types and model details.
use super::stages::{Block, CaptureStage};
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};

pub struct DeepFilter {
    model: DfTract,
    pristine: DfTract,
    delay: usize,
}
impl DeepFilter {
    pub fn new() -> Result<Self, String> {
        // Model bytes are bundled by the pinned dependency. No runtime download,
        // Python, external process, device access or user-supplied model path.
        let pristine = DfTract::new(DfParams::default(), &RuntimeParams::default())
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
    fn process(&mut self, block: &mut Block) -> Result<(), String> {
        let mut out = [0.0; 480];
        self.model
            .process(
                ArrayView2::from_shape((1, 480), &block[..]).map_err(|e| e.to_string())?,
                ArrayViewMut2::from_shape((1, 480), &mut out[..]).map_err(|e| e.to_string())?,
            )
            .map_err(|_| "Inference failed; audio stopped. Disable suppression and reconnect.")?;
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
}
