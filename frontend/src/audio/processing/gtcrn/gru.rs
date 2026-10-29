//! Native ONNX GRU for tract. tract-onnx 0.21.4 lowers each GRU to a Scan whose
//! per-step interpreter overhead dominates GTCRN's tiny recurrences (8 GRUs x 33
//! frequency steps per frame, hidden size 4): about 14 ms per 16 ms frame. This
//! op evaluates the same ONNX equations in a direct loop; tests check parity with
//! tract's Scan lowering. Unsupported attributes are rejected, not approximated.
use tract_hir::internal::*;
use tract_onnx::{model::ParsingContext, pb::NodeProto};

pub fn register(onnx: &mut tract_onnx::Onnx) {
    onnx.op_register.insert("GRU", parse);
}

fn parse(_: &ParsingContext, node: &NodeProto) -> TractResult<(Box<dyn InferenceOp>, Vec<String>)> {
    let present = |names: &[String], index: usize| names.get(index).is_some_and(|s| !s.is_empty());
    ensure!(
        node.get_attr_opt::<i64>("layout")?.unwrap_or(0) == 0,
        "batch-first GRU is not supported"
    );
    ensure!(
        node.attribute.iter().all(|a| !matches!(
            a.name.as_str(),
            "activations" | "activation_alpha" | "activation_beta" | "clip"
        )),
        "custom GRU activations or clipping are not supported"
    );
    ensure!(
        !present(&node.input, 4),
        "GRU sequence_lens is not supported"
    );
    let direction = node.get_attr_opt::<&str>("direction")?.unwrap_or("forward");
    ensure!(
        matches!(direction, "forward" | "bidirectional"),
        "GRU direction {direction} is not supported"
    );
    let gru = Gru {
        op: GruOp {
            linear_before_reset: node
                .get_attr_opt::<i64>("linear_before_reset")?
                .unwrap_or(0)
                != 0,
            bias: present(&node.input, 3),
            initial_h: present(&node.input, 5),
        },
        y: present(&node.output, 0),
        y_h: present(&node.output, 1),
    };
    Ok((expand(gru), vec![]))
}

#[derive(Debug, Clone)]
struct Gru {
    op: GruOp,
    y: bool,
    y_h: bool,
}
impl Expansion for Gru {
    fn name(&self) -> Cow<'_, str> {
        "NativeGru".into()
    }
    fn nboutputs(&self) -> TractResult<usize> {
        Ok(self.y as usize + self.y_h as usize)
    }
    fn rules<'r, 'p: 'r, 's: 'r>(
        &'s self,
        s: &mut Solver<'r>,
        inputs: &'p [TensorProxy],
        outputs: &'p [TensorProxy],
    ) -> InferenceResult {
        check_input_arity(
            inputs,
            3 + self.op.bias as usize + self.op.initial_h as usize,
        )?;
        check_output_arity(outputs, self.nboutputs()?)?;
        // X [seq, batch, input], W [dirs, 3H, input], R [dirs, 3H, H]
        for input in inputs {
            s.equals(&input.datum_type, f32::datum_type())?;
        }
        s.equals(&inputs[0].rank, 3)?;
        s.equals(&inputs[1].rank, 3)?;
        s.equals(&inputs[2].rank, 3)?;
        s.equals(&inputs[1].shape[0], &inputs[2].shape[0])?;
        s.equals(&inputs[1].shape[2], &inputs[0].shape[2])?;
        s.equals(&inputs[1].shape[1], 3 * inputs[2].shape[2].bex())?;
        s.equals(&inputs[2].shape[1], 3 * inputs[2].shape[2].bex())?;
        if self.op.bias {
            s.equals(&inputs[3].rank, 2)?;
            s.equals(&inputs[3].shape[0], &inputs[1].shape[0])?;
            s.equals(&inputs[3].shape[1], 6 * inputs[2].shape[2].bex())?;
        }
        if self.op.initial_h {
            let h = &inputs[3 + self.op.bias as usize];
            s.equals(&h.rank, 3)?;
            s.equals(&h.shape[0], &inputs[1].shape[0])?;
            s.equals(&h.shape[1], &inputs[0].shape[1])?;
            s.equals(&h.shape[2], &inputs[2].shape[2])?;
        }
        let mut output = outputs.iter();
        if self.y {
            let y = output.next().unwrap();
            s.equals(&y.datum_type, f32::datum_type())?;
            s.equals(&y.rank, 4)?;
            s.equals(&y.shape[0], &inputs[0].shape[0])?;
            s.equals(&y.shape[1], &inputs[1].shape[0])?;
            s.equals(&y.shape[2], &inputs[0].shape[1])?;
            s.equals(&y.shape[3], &inputs[2].shape[2])?;
        }
        if self.y_h {
            let y_h = output.next().unwrap();
            s.equals(&y_h.datum_type, f32::datum_type())?;
            s.equals(&y_h.rank, 3)?;
            s.equals(&y_h.shape[0], &inputs[1].shape[0])?;
            s.equals(&y_h.shape[1], &inputs[0].shape[1])?;
            s.equals(&y_h.shape[2], &inputs[2].shape[2])?;
        }
        Ok(())
    }
    fn wire(
        &self,
        prefix: &str,
        model: &mut TypedModel,
        inputs: &[OutletId],
    ) -> TractResult<TVec<OutletId>> {
        let [y, y_h] = model.wire_node(prefix, self.op.clone(), inputs)?[..] else {
            bail!("NativeGru must produce Y and Y_h");
        };
        let mut wired = tvec![];
        if self.y {
            wired.push(y);
        }
        if self.y_h {
            wired.push(y_h);
        }
        Ok(wired)
    }
}

#[derive(Debug, Clone)]
struct GruOp {
    linear_before_reset: bool,
    bias: bool,
    initial_h: bool,
}
impl Op for GruOp {
    fn name(&self) -> Cow<'_, str> {
        "NativeGru".into()
    }
    op_as_typed_op!();
}
impl TypedOp for GruOp {
    as_op!();
    fn output_facts(&self, inputs: &[&TypedFact]) -> TractResult<TVec<TypedFact>> {
        let (x, w, r) = (&inputs[0].shape, &inputs[1].shape, &inputs[2].shape);
        Ok(tvec![
            f32::fact([x[0].clone(), w[0].clone(), x[1].clone(), r[2].clone()]),
            f32::fact([w[0].clone(), x[1].clone(), r[2].clone()]),
        ])
    }
}
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
impl EvalOp for GruOp {
    fn is_stateless(&self) -> bool {
        true
    }
    fn eval(&self, inputs: TVec<TValue>) -> TractResult<TVec<TValue>> {
        let &[steps, batch, input] = inputs[0].shape() else {
            bail!("GRU input must have rank 3");
        };
        let (dirs, hidden) = (inputs[1].shape()[0], inputs[2].shape()[2]);
        let x = inputs[0].as_slice::<f32>()?;
        let w = inputs[1].as_slice::<f32>()?;
        let r = inputs[2].as_slice::<f32>()?;
        let b = self.bias.then(|| inputs[3].as_slice::<f32>()).transpose()?;
        let h0 = self
            .initial_h
            .then(|| inputs[3 + self.bias as usize].as_slice::<f32>())
            .transpose()?;
        ensure!(w.len() == dirs * 3 * hidden * input && r.len() == dirs * 3 * hidden * hidden);
        let mut y = vec![0.0; steps * dirs * batch * hidden];
        let mut y_h = vec![0.0; dirs * batch * hidden];
        let (mut h, mut next) = (vec![0.0; hidden], vec![0.0; hidden]);
        let (mut reset, mut gated) = (vec![0.0; hidden], vec![0.0; hidden]);
        for d in 0..dirs {
            let w = &w[d * 3 * hidden * input..][..3 * hidden * input];
            let r = &r[d * 3 * hidden * hidden..][..3 * hidden * hidden];
            // Gate order is z, r, h; B = [Wb_z, Wb_r, Wb_h, Rb_z, Rb_r, Rb_h].
            let bias = |gate: usize| b.map_or(0.0, |b| b[d * 6 * hidden + gate]);
            let (wx, rh) = (
                |g: usize, xt: &[f32]| dot(&w[g * input..][..input], xt),
                |g: usize, h: &[f32]| dot(&r[g * hidden..][..hidden], h),
            );
            for n in 0..batch {
                match h0 {
                    Some(h0) => h.copy_from_slice(&h0[(d * batch + n) * hidden..][..hidden]),
                    None => h.fill(0.0),
                }
                for step in 0..steps {
                    let t = if d == 0 { step } else { steps - 1 - step };
                    let xt = &x[(t * batch + n) * input..][..input];
                    for j in 0..hidden {
                        let g = hidden + j;
                        reset[j] = sigmoid(wx(g, xt) + rh(g, &h) + bias(g) + bias(3 * hidden + g));
                        gated[j] = reset[j] * h[j];
                    }
                    for j in 0..hidden {
                        let (z, c) = (j, 2 * hidden + j);
                        let update =
                            sigmoid(wx(z, xt) + rh(z, &h) + bias(z) + bias(3 * hidden + z));
                        let recurrent = if self.linear_before_reset {
                            reset[j] * (rh(c, &h) + bias(3 * hidden + c))
                        } else {
                            rh(c, &gated) + bias(3 * hidden + c)
                        };
                        let candidate = (wx(c, xt) + bias(c) + recurrent).tanh();
                        next[j] = (1.0 - update) * candidate + update * h[j];
                    }
                    std::mem::swap(&mut h, &mut next);
                    y[((t * dirs + d) * batch + n) * hidden..][..hidden].copy_from_slice(&h);
                }
                y_h[(d * batch + n) * hidden..][..hidden].copy_from_slice(&h);
            }
        }
        Ok(tvec![
            Tensor::from_shape(&[steps, dirs, batch, hidden], &y)?.into_tvalue(),
            Tensor::from_shape(&[dirs, batch, hidden], &y_h)?.into_tvalue(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tract_onnx::pb::*;
    use tract_onnx::prelude::Framework;

    fn tensor(name: &str, dims: &[i64], seed: &mut u32) -> TensorProto {
        let len = dims.iter().product::<i64>() as usize;
        TensorProto {
            name: name.into(),
            dims: dims.into(),
            data_type: 1,
            float_data: (0..len)
                .map(|_| {
                    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (*seed as f32 / u32::MAX as f32 - 0.5) * 1.5
                })
                .collect(),
            ..Default::default()
        }
    }
    fn plan(
        onnx: &tract_onnx::Onnx,
        dirs: i64,
        lbr: i64,
        seed: u32,
    ) -> TypedSimplePlan<TypedModel> {
        let (hidden, input) = (4_i64, 3_i64);
        let mut seed = seed;
        let attr = |name: &str, i: i64| AttributeProto {
            name: name.into(),
            r#type: 2,
            i,
            ..Default::default()
        };
        let direction = AttributeProto {
            name: "direction".into(),
            r#type: 3,
            s: if dirs == 2 {
                b"bidirectional".to_vec()
            } else {
                b"forward".to_vec()
            },
            ..Default::default()
        };
        let value = |name: &str| ValueInfoProto {
            name: name.into(),
            r#type: Some(TypeProto {
                value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                    elem_type: 1,
                    shape: None,
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let graph = GraphProto {
            name: "gru".into(),
            node: vec![NodeProto {
                name: "gru".into(),
                op_type: "GRU".into(),
                input: ["x", "w", "r", "b", "", "h0"].map(String::from).into(),
                output: ["y", "y_h"].map(String::from).into(),
                attribute: vec![
                    attr("hidden_size", hidden),
                    attr("linear_before_reset", lbr),
                    direction,
                ],
                ..Default::default()
            }],
            initializer: vec![
                tensor("w", &[dirs, 3 * hidden, input], &mut seed),
                tensor("r", &[dirs, 3 * hidden, hidden], &mut seed),
                tensor("b", &[dirs, 6 * hidden], &mut seed),
            ],
            input: vec![value("x"), value("h0")],
            output: vec![value("y"), value("y_h")],
            ..Default::default()
        };
        let proto = ModelProto {
            ir_version: 7,
            opset_import: vec![OperatorSetIdProto {
                domain: String::new(),
                version: 11,
            }],
            graph: Some(graph),
            ..Default::default()
        };
        onnx.model_for_proto_model(&proto)
            .unwrap()
            .with_input_fact(0, f32::fact([5, 2, input as usize]).into())
            .unwrap()
            .with_input_fact(1, f32::fact([dirs as usize, 2, hidden as usize]).into())
            .unwrap()
            .into_optimized()
            .unwrap()
            .into_runnable()
            .unwrap()
    }

    #[test]
    fn native_gru_matches_scan_with_initial_state() {
        let mut native_onnx = tract_onnx::onnx();
        register(&mut native_onnx);
        for dirs in [1, 2] {
            for lbr in [0, 1] {
                for zero_h in [true, false] {
                    let native = plan(&native_onnx, dirs, lbr, 5);
                    let reference = plan(&tract_onnx::onnx(), dirs, lbr, 5);
                    let mut seed = 77;
                    let x = tensor("x", &[5, 2, 3], &mut seed);
                    let h = tensor("h0", &[dirs, 2, 4], &mut seed);
                    let x = Tensor::from_shape(&[5, 2, 3], &x.float_data).unwrap();
                    let h = if zero_h {
                        Tensor::zero::<f32>(&[dirs as usize, 2, 4]).unwrap()
                    } else {
                        Tensor::from_shape(&[dirs as usize, 2, 4], &h.float_data).unwrap()
                    };
                    let a = native
                        .run(tvec![x.clone().into_tvalue(), h.clone().into_tvalue()])
                        .unwrap();
                    let b = reference
                        .run(tvec![x.into_tvalue(), h.into_tvalue()])
                        .unwrap();
                    for (a, b) in a.iter().zip(&b) {
                        let (a, b) = (a.as_slice::<f32>().unwrap(), b.as_slice::<f32>().unwrap());
                        assert_eq!(a.len(), b.len());
                        for (a, b) in a.iter().zip(b) {
                            assert!(
                                (a - b).abs() < 1e-5,
                                "dirs {dirs}, linear_before_reset {lbr}, zero_h {zero_h}: {a} vs {b}"
                            );
                        }
                    }
                }
            }
        }
    }
}
