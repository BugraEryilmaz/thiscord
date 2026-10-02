//! Narrow, hash-pinned execution of the experimental REE v2 graph.
//! Float activations, hybrid int8 fully-connected weights; no general TFLite
//! interpreter or graph rewriting. All buffers and index maps are built at load.
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, sync::Arc};
use tract_tflite::{
    internal::{TractResult, ensure},
    tflite as fb,
};

pub const MODEL_SHA256: &str = "3a18833eaeb08bfffb88a588c10db67885f246ba4794fd0f1609f2ccf6c30b77";
pub const BINS: usize = 129;
pub const STATE: usize = 864;

#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    len: usize,
}
impl Span {
    fn range(self) -> std::ops::Range<usize> {
        self.start..self.start + self.len
    }
}
#[derive(Debug)]
enum Step {
    Copy(Vec<(usize, usize)>),
    Unary {
        input: Span,
        output: Span,
        sigmoid: bool,
    },
    Binary {
        a: Span,
        b: Span,
        output: Span,
        op: fb::BuiltinOperator,
    },
    Dense {
        input: Span,
        output: Span,
        weights: Vec<i8>,
        scales: Vec<f32>,
        bias: Option<Span>,
    },
}
#[derive(Debug)]
struct Plan {
    steps: Vec<Step>,
    seed: Vec<f32>,
    inputs: [Span; 4],
    outputs: [Span; 3],
    scratch: usize,
}
#[derive(Clone)]
pub struct Model {
    plan: Arc<Plan>,
    arena: Vec<f32>,
    quantized: Vec<i32>,
}
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Activations and recurrent state contain microphone-derived features.
        f.debug_struct("Model")
            .field("sha256", &MODEL_SHA256)
            .finish_non_exhaustive()
    }
}
impl Model {
    /// Embedded in native builds so installed clients need no external model file.
    pub fn bundled() -> Result<Self, String> {
        Self::from_bytes(include_bytes!("../../../../models/ree-v2.tflite"))
            .map_err(|e| format!("Bundled neural echo model: {e}"))
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(425_265).read_to_end(&mut bytes))
            .map_err(|_| "Cannot read neural echo model file".to_string())?;
        Self::from_bytes(&bytes).map_err(|e| format!("Neural echo model: {e}"))
    }
    fn from_bytes(bytes: &[u8]) -> TractResult<Self> {
        ensure!(
            bytes.len() == 425_264 && format!("{:x}", Sha256::digest(bytes)) == MODEL_SHA256,
            "unsupported model; expected the documented REE v2 SHA-256"
        );
        let root = fb::root_as_model(bytes)?;
        let graph = root.subgraphs().unwrap().get(0);
        let tensors = graph.tensors().unwrap();
        let buffers = root.buffers().unwrap();
        let mut seed = Vec::new();
        let mut spans = Vec::new();
        let mut shapes = Vec::new();
        let mut ints = vec![Vec::<i32>::new(); tensors.len()];
        for (i, t) in tensors.iter().enumerate() {
            let shape: Vec<usize> = t.shape().unwrap().iter().map(|n| n as usize).collect();
            let len = shape.iter().product();
            let span = Span {
                start: seed.len(),
                len,
            };
            seed.resize(seed.len() + len, 0.0);
            if let Some(data) = buffers.get(t.buffer() as usize).data() {
                let data = data.bytes();
                if t.type_() == fb::TensorType::FLOAT32 {
                    ensure!(data.len() == len * 4, "constant shape mismatch");
                    for (v, b) in seed[span.range()].iter_mut().zip(data.as_chunks::<4>().0) {
                        *v = f32::from_le_bytes(*b);
                    }
                } else if t.type_() == fb::TensorType::INT32 {
                    ints[i] = data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| i32::from_le_bytes(*b))
                        .collect();
                }
            }
            spans.push(span);
            shapes.push(shape);
        }
        // Names, state layout and metadata are fixed by the hash above. Keep
        // explicit contract checks to catch mistakes in the adapter itself.
        let sig = root.signature_defs().unwrap().get(0);
        let input = |name: &str| {
            spans[sig
                .inputs()
                .unwrap()
                .iter()
                .find(|s| s.name() == Some(name))
                .unwrap()
                .tensor_index() as usize]
        };
        let output = |name: &str| {
            spans[sig
                .outputs()
                .unwrap()
                .iter()
                .find(|s| s.name() == Some(name))
                .unwrap()
                .tensor_index() as usize]
        };
        let inputs = [
            input("mic_frame"),
            input("cancelled_frame"),
            input("ref_frame"),
            input("lstm_state"),
        ];
        let outputs = [
            output("echo_mask_frame"),
            output("unbounded_echo_mask_frame"),
            output("lstm_state"),
        ];
        ensure!(
            inputs.map(|s| s.len) == [BINS, BINS, BINS, STATE]
                && outputs.map(|s| s.len) == [BINS, BINS, STATE],
            "model contract mismatch"
        );
        let mut steps = Vec::new();
        let mut scratch = 0;
        for operator in graph.operators().unwrap() {
            let kind = root
                .operator_codes()
                .unwrap()
                .get(operator.opcode_index() as usize)
                .builtin_code();
            let ids: Vec<i32> = operator.inputs().unwrap().iter().collect();
            let out_id = operator.outputs().unwrap().get(0) as usize;
            let out = spans[out_id];
            let a = spans[ids[0] as usize];
            use fb::BuiltinOperator as Op;
            let step = match kind {
                Op::RESHAPE => {
                    ensure!(out.len == a.len, "reshape mismatch");
                    Step::Copy((0..out.len).map(|i| (out.start + i, a.start + i)).collect())
                }
                Op::SLICE => {
                    let begin = &ints[ids[1] as usize];
                    let input_shape = &shapes[ids[0] as usize];
                    let output_shape = &shapes[out_id];
                    let mut pairs = Vec::with_capacity(out.len);
                    for i in 0..out.len {
                        let mut index = i;
                        let mut source = 0;
                        let mut stride = 1;
                        for axis in (0..input_shape.len()).rev() {
                            source += (index % output_shape[axis] + begin[axis] as usize) * stride;
                            index /= output_shape[axis];
                            stride *= input_shape[axis];
                        }
                        ensure!(source < a.len, "slice mismatch");
                        pairs.push((out.start + i, a.start + source));
                    }
                    Step::Copy(pairs)
                }
                Op::CONCATENATION => {
                    let opt = operator.builtin_options_as_concatenation_options().unwrap();
                    ensure!(
                        opt.fused_activation_function() == fb::ActivationFunctionType::NONE,
                        "fused concat"
                    );
                    let rank = shapes[out_id].len();
                    let axis = opt.axis().rem_euclid(rank as i32) as usize;
                    let outer: usize = shapes[out_id][..axis].iter().product();
                    let mut pairs = Vec::with_capacity(out.len);
                    for n in 0..outer {
                        for &id in &ids {
                            let span = spans[id as usize];
                            let chunk = span.len / outer;
                            for j in 0..chunk {
                                pairs.push((out.start + pairs.len(), span.start + n * chunk + j));
                            }
                        }
                    }
                    ensure!(pairs.len() == out.len, "concat mismatch");
                    Step::Copy(pairs)
                }
                Op::LOGISTIC | Op::TANH => {
                    ensure!(out.len == a.len, "unary mismatch");
                    Step::Unary {
                        input: a,
                        output: out,
                        sigmoid: kind == Op::LOGISTIC,
                    }
                }
                Op::FULLY_CONNECTED => {
                    let opt = operator
                        .builtin_options_as_fully_connected_options()
                        .unwrap();
                    ensure!(
                        opt.fused_activation_function() == fb::ActivationFunctionType::NONE
                            && opt.asymmetric_quantize_inputs(),
                        "dense options"
                    );
                    let weight = tensors.get(ids[1] as usize);
                    ensure!(
                        weight.type_() == fb::TensorType::INT8,
                        "expected hybrid weights"
                    );
                    let weights: Vec<i8> = buffers
                        .get(weight.buffer() as usize)
                        .data()
                        .unwrap()
                        .iter()
                        .map(|v| v as i8)
                        .collect();
                    let q = weight.quantization().unwrap();
                    let scales: Vec<f32> = q.scale().unwrap().iter().collect();
                    ensure!(
                        weights.len() == a.len * out.len
                            && scales.len() == out.len
                            && q.zero_point().unwrap().iter().all(|v| v == 0),
                        "dense shape/quantization"
                    );
                    scratch = scratch.max(a.len);
                    let bias = if ids[2] < 0 {
                        None
                    } else {
                        Some(spans[ids[2] as usize])
                    };
                    ensure!(bias.is_none_or(|b| b.len == out.len), "bias shape");
                    Step::Dense {
                        input: a,
                        output: out,
                        weights,
                        scales,
                        bias,
                    }
                }
                Op::ADD | Op::SUB | Op::MUL | Op::MINIMUM | Op::MAXIMUM => {
                    let b = spans[ids[1] as usize];
                    ensure!(
                        (a.len == 1 || a.len == out.len) && (b.len == 1 || b.len == out.len),
                        "unsupported broadcast"
                    );
                    Step::Binary {
                        a,
                        b,
                        output: out,
                        op: kind,
                    }
                }
                _ => {
                    return Err(tract_tflite::internal::format_err!(
                        "unsupported operator {kind:?}"
                    ));
                }
            };
            steps.push(step);
        }
        let plan = Arc::new(Plan {
            steps,
            seed,
            inputs,
            outputs,
            scratch,
        });
        Ok(Self {
            arena: plan.seed.clone(),
            quantized: vec![0; plan.scratch],
            plan,
        })
    }
    pub fn reset(&mut self) {
        self.arena.copy_from_slice(&self.plan.seed);
        self.quantized.fill(0);
    }
    pub fn infer(
        &mut self,
        cancelled: &[f32; BINS],
        reference: &[f32; BINS],
    ) -> Result<([f32; BINS], [f32; BINS]), String> {
        if cancelled.iter().chain(reference).any(|v| !v.is_finite()) {
            return Err("Non-finite neural echo features".into());
        }
        self.arena[self.plan.inputs[1].range()].copy_from_slice(cancelled);
        self.arena[self.plan.inputs[2].range()].copy_from_slice(reference);
        // Upstream v2 feature extraction intentionally leaves mic_frame zero.
        for step in &self.plan.steps {
            match step {
                Step::Copy(pairs) => {
                    for &(to, from) in pairs {
                        self.arena[to] = self.arena[from];
                    }
                }
                Step::Unary {
                    input,
                    output,
                    sigmoid,
                } => {
                    for i in 0..output.len {
                        let x = self.arena[input.start + i];
                        self.arena[output.start + i] = if *sigmoid {
                            1.0 / (1.0 + (-x).exp())
                        } else {
                            x.tanh()
                        };
                    }
                }
                Step::Binary { a, b, output, op } => {
                    for i in 0..output.len {
                        let x = self.arena[a.start + i % a.len];
                        let y = self.arena[b.start + i % b.len];
                        use fb::BuiltinOperator as Op;
                        self.arena[output.start + i] = match *op {
                            Op::ADD => x + y,
                            Op::SUB => x - y,
                            Op::MUL => x * y,
                            Op::MINIMUM => x.min(y),
                            Op::MAXIMUM => x.max(y),
                            _ => unreachable!(),
                        };
                    }
                }
                Step::Dense {
                    input,
                    output,
                    weights,
                    scales,
                    bias,
                } => {
                    let (scale, zero) =
                        quantize(&self.arena[input.range()], &mut self.quantized[..input.len]);
                    for row in 0..output.len {
                        let dot: i32 = weights[row * input.len..(row + 1) * input.len]
                            .iter()
                            .zip(&self.quantized)
                            .map(|(&w, &x)| i32::from(w) * (x - zero))
                            .sum();
                        self.arena[output.start + row] = dot as f32 * (scale * scales[row])
                            + bias.map_or(0.0, |b| self.arena[b.start + row]);
                    }
                }
            }
        }
        if self
            .plan
            .outputs
            .iter()
            .any(|s| self.arena[s.range()].iter().any(|v| !v.is_finite()))
        {
            return Err("Non-finite neural echo inference".into());
        }
        let masks = (
            self.arena[self.plan.outputs[0].range()].try_into().unwrap(),
            self.arena[self.plan.outputs[1].range()].try_into().unwrap(),
        );
        for i in 0..STATE {
            self.arena[self.plan.inputs[3].start + i] =
                self.arena[self.plan.outputs[2].start + i] * 0.999;
        }
        Ok(masks)
    }
    pub fn state(&self) -> &[f32] {
        &self.arena[self.plan.inputs[3].range()]
    }
    /// Reference-harness only: compare one transition from an identical state.
    #[cfg(feature = "neural-echo-probe")]
    pub fn set_reference_state(&mut self, state: &[f32]) -> Result<(), String> {
        if state.len() != STATE || state.iter().any(|v| !v.is_finite()) {
            return Err("Invalid reference state".into());
        }
        self.arena[self.plan.inputs[3].range()].copy_from_slice(state);
        Ok(())
    }
}

// TFLite's asymmetric float->int8 convention: range includes zero, per-vector
// Adapted from TensorFlow portable tensor utilities, Copyright 2019 The
// TensorFlow Authors, Apache-2.0 (vendor/TENSORFLOW_LICENSE.txt), modified for Rust.
// scale, nearest integer ties away from zero. Int32 dot products apply the input
// zero point before per-output-channel weight scales and float bias. Round the
// scaled value BEFORE adding the offset; translating a ties-away rounding
// operation changes its result across zero (covered by the regression below).
fn quantize(input: &[f32], output: &mut [i32]) -> (f32, i32) {
    let low = input.iter().copied().fold(0.0_f32, f32::min) as f64;
    let high = input.iter().copied().fold(0.0_f32, f32::max) as f64;
    if low == high {
        output.fill(0);
        return (1.0, 0);
    }
    let scale = (high - low) / 255.0;
    let min_zero = -128.0 - low / scale;
    let max_zero = 127.0 - high / scale;
    let zero = if 128.0 + (low / scale).abs() < 127.0 + (high / scale).abs() {
        min_zero
    } else {
        max_zero
    };
    let zero = zero.round().clamp(-128.0, 127.0) as i32;
    let scale = scale as f32;
    let inv = 1.0 / scale;
    for (&x, y) in input.iter().zip(output) {
        *y = (zero as f32 + (x * inv).round()).clamp(-128.0, 127.0) as i32;
    }
    (scale, zero)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unpinned_or_corrupt_weights() {
        assert!(Model::from_bytes(b"bad").is_err());
    }
    #[test]
    fn hybrid_quantization_preserves_zero_and_range() {
        for input in [
            [0.0; 4],
            [-1.0, 0.0, 0.5, 1.0],
            [0.0, 0.1, 1.0, 2.0],
            [-2.0, -1.0, -0.1, 0.0],
        ] {
            let mut q = [0; 4];
            let (s, z) = quantize(&input, &mut q);
            for (x, q) in input.into_iter().zip(q) {
                assert!((x - (q - z) as f32 * s).abs() <= s * 0.51);
            }
        }
    }
    #[test]
    fn quantization_rounds_before_adding_negative_zero_point() {
        let mut q = [0; 3];
        let (scale, zero) = quantize(&[0.0, 63.5, 255.0], &mut q);
        assert_eq!(scale, 1.0);
        assert_eq!(zero, -128);
        assert_eq!(q, [-128, -64, 127]);
    }
}
