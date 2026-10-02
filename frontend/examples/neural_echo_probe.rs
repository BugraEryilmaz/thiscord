//! Offline recurrent-model parity and timing. No devices or model downloads.
use std::{hint::black_box, path::Path, time::Instant};
use thiscord_frontend::audio::processing::neural_echo::model::{BINS, MODEL_SHA256, Model};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 3 {
        return Err(
            "Usage: neural_echo_probe <model.tflite> [iterations:100..100000] [reference.json]"
                .into(),
        );
    }
    let iterations: usize = args.get(1).map(|v| v.parse()).transpose()?.unwrap_or(1000);
    if !(100..=100_000).contains(&iterations) {
        return Err("Iterations must be between 100 and 100000".into());
    }
    let start = Instant::now();
    let mut model = Model::load(Path::new(&args[0]))?;
    let initialization_ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Some(path) = args.get(2) {
        let reference: serde_json::Value = serde_json::from_reader(std::fs::File::open(path)?)?;
        if reference["sha256"] != MODEL_SHA256 {
            return Err("Reference model hash differs".into());
        }
        let mut maximum = 0.0_f32;
        let mut transition_max = 0.0_f32;
        let mut rollout_mask_max = 0.0_f32;
        let mut aligned = model.clone();
        let frames = reference["frames"].as_array().ok_or("Missing frames")?;
        if frames.len() != 100 {
            return Err("Expected the complete 100-frame reference sequence".into());
        }
        for frame in frames {
            let decode = |key: &str| -> Result<[f32; BINS], Box<dyn std::error::Error>> {
                let bits: Vec<u32> = serde_json::from_value(frame[key].clone())?;
                let values: Vec<f32> = bits.into_iter().map(f32::from_bits).collect();
                Ok(values.try_into().map_err(|_| "Wrong feature size")?)
            };
            let (mask, unbounded) = model.infer(&decode("cancelled")?, &decode("reference")?)?;
            let bits: Vec<u32> = serde_json::from_value(frame["state"].clone())?;
            let state: Vec<f32> = bits.into_iter().map(f32::from_bits).collect();
            let (am, au) = aligned.infer(&decode("cancelled")?, &decode("reference")?)?;
            for (got, expected) in am.iter().chain(&au).chain(aligned.state()).zip(
                decode("mask")?
                    .iter()
                    .chain(decode("unbounded")?.iter())
                    .chain(&state),
            ) {
                transition_max = transition_max.max((got - expected).abs());
            }
            aligned.set_reference_state(&state)?;
            for (got, expected) in mask
                .iter()
                .chain(&unbounded)
                .zip(decode("mask")?.iter().chain(decode("unbounded")?.iter()))
            {
                rollout_mask_max = rollout_mask_max.max((got - expected).abs());
            }
            for (got, expected) in mask.iter().chain(&unbounded).chain(model.state()).zip(
                decode("mask")?
                    .iter()
                    .chain(decode("unbounded")?.iter())
                    .chain(&state),
            ) {
                maximum = maximum.max((got - expected).abs());
            }
        }
        println!(
            "{}",
            serde_json::json!({"rollout_max_abs_error_including_state":maximum,"rollout_mask_max_abs_error":rollout_mask_max,"identical_state_transition_max_abs_error":transition_max})
        );
        if maximum > 0.0001 || transition_max > 0.0001 || rollout_mask_max > 0.0001 {
            return Err("Reference parity failed".into());
        }
    }
    model.reset();
    let mut times = Vec::with_capacity(iterations);
    for i in 0..iterations + 20 {
        let input = std::array::from_fn(|j| ((i * 17 + j * 31) % 997) as f32 / 997.0);
        let reference = std::array::from_fn(|j| ((i * 23 + j * 7) % 991) as f32 / 991.0);
        let start = Instant::now();
        black_box(model.infer(&input, &reference)?);
        if i >= 20 {
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({
            "scope":"recurrent REE model only; excludes features, AEC3 and devices",
            "model_sha256":MODEL_SHA256,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "iterations":iterations,"initialization_ms":initialization_ms,
            "mean_ms":times.iter().sum::<f64>()/iterations as f64,"p50_ms":times[iterations/2],
            "p95_ms":times[iterations*95/100],"p99_ms":times[iterations*99/100],"max_ms":times[iterations-1]
        })
    );
    Ok(())
}
