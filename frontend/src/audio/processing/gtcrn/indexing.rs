//! Fast f32 evaluation for tract's Gather/ScatterNd at GTCRN's cache shapes.
//! tract 0.21.4's generic paths cost about 7 ms per 16 ms frame here: cache
//! updates scatter thousands of elements through constant coordinate tables,
//! and scalar-index gathers walk elements individually. Nodes keep tract's
//! inferred facts and optimizations; only evaluation is replaced, and other
//! datum types still use tract's implementation.
use tract_onnx::prelude::*;
use tract_onnx::tract_core::internal::*;
use tract_onnx::tract_core::ops::array::{Gather, ScatterNd};

pub fn accelerate(model: &mut TypedModel) {
    for node in &mut model.nodes {
        if let Some(gather) = node.op_as::<Gather>() {
            node.op = Box::new(FastGather(gather.clone()));
        } else if node.op_is::<ScatterNd>() {
            node.op = Box::new(FastScatterNd);
        }
    }
}
fn checked(index: i64, dim: usize) -> TractResult<usize> {
    let index = if index < 0 { index + dim as i64 } else { index };
    ensure!(
        (0..dim as i64).contains(&index),
        "index {index} out of range {dim}"
    );
    Ok(index as usize)
}

#[derive(Debug, Clone)]
struct FastGather(Gather);
impl Op for FastGather {
    fn name(&self) -> Cow<'_, str> {
        "FastGather".into()
    }
    op_as_typed_op!();
}
impl TypedOp for FastGather {
    as_op!();
    fn output_facts(&self, inputs: &[&TypedFact]) -> TractResult<TVec<TypedFact>> {
        self.0.output_facts(inputs)
    }
}
impl EvalOp for FastGather {
    fn is_stateless(&self) -> bool {
        true
    }
    fn eval(&self, inputs: TVec<TValue>) -> TractResult<TVec<TValue>> {
        if inputs[0].datum_type() != f32::datum_type() {
            return self.0.eval(inputs);
        }
        let shape = inputs[0].shape();
        let axis = self.0.axis;
        let (outer, dim) = (shape[..axis].iter().product::<usize>(), shape[axis]);
        let inner = shape[axis + 1..].iter().product::<usize>();
        let source = inputs[0].as_slice::<f32>()?;
        let indices = inputs[1].cast_to::<i64>()?;
        let indices = indices.as_slice::<i64>()?;
        let mut output = Vec::with_capacity(outer * indices.len() * inner);
        for block in 0..outer {
            for &index in indices {
                let start = (block * dim + checked(index, dim)?) * inner;
                output.extend_from_slice(&source[start..][..inner]);
            }
        }
        let mut output_shape: TVec<usize> = shape[..axis].into();
        output_shape.extend_from_slice(inputs[1].shape());
        output_shape.extend_from_slice(&shape[axis + 1..]);
        Ok(tvec![
            Tensor::from_shape(&output_shape, &output)?.into_tvalue()
        ])
    }
}

#[derive(Debug, Clone)]
struct FastScatterNd;
impl Op for FastScatterNd {
    fn name(&self) -> Cow<'_, str> {
        "FastScatterNd".into()
    }
    op_as_typed_op!();
}
impl TypedOp for FastScatterNd {
    as_op!();
    fn output_facts(&self, inputs: &[&TypedFact]) -> TractResult<TVec<TypedFact>> {
        ScatterNd.output_facts(inputs)
    }
}
impl EvalOp for FastScatterNd {
    fn is_stateless(&self) -> bool {
        true
    }
    fn eval(&self, inputs: TVec<TValue>) -> TractResult<TVec<TValue>> {
        if inputs[0].datum_type() != f32::datum_type()
            || inputs[2].datum_type() != f32::datum_type()
        {
            return ScatterNd.eval(inputs);
        }
        // indices [..., k] address the first k axes; each selects a slice of
        // the remaining axes, which `updates` supplies in index order.
        let shape = inputs[0].shape();
        let depth = *inputs[1]
            .shape()
            .last()
            .context("ScatterND indices rank 0")?;
        ensure!(
            (1..=shape.len()).contains(&depth),
            "ScatterND index depth {depth}"
        );
        let inner = shape[depth..].iter().product::<usize>();
        ensure!(inner > 0, "ScatterND on an empty slice");
        let strides: TVec<usize> = (0..depth)
            .map(|axis| shape[axis + 1..].iter().product())
            .collect();
        let indices = inputs[1].cast_to::<i64>()?;
        let indices = indices.as_slice::<i64>()?;
        let updates = inputs[2].as_slice::<f32>()?;
        ensure!(
            updates.len() == indices.len() / depth * inner,
            "ScatterND update size"
        );
        let mut output = inputs[0].as_slice::<f32>()?.to_vec();
        for (update, coordinates) in updates.chunks_exact(inner).zip(indices.chunks_exact(depth)) {
            let mut offset = 0;
            for ((&index, &dim), stride) in coordinates.iter().zip(shape).zip(&strides) {
                offset += checked(index, dim)? * stride;
            }
            output[offset..][..inner].copy_from_slice(update);
        }
        Ok(tvec![Tensor::from_shape(shape, &output)?.into_tvalue()])
    }
}
