//! Offline A/B fixture: playback echo, then double-talk. No audio devices.
#[path = "../tests/support/audio_fixture.rs"]
#[allow(dead_code)]
mod fixture;
use std::{path::Path, time::Instant};
use thiscord_frontend::audio::processing::Processing;
use thiscord_shared::audio::{AudioSettings, NoiseSuppressionModel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 1 || !Path::new(&args[0]).is_file() {
        return Err("Usage: neural_echo_bench <model.tflite>".into());
    }
    let speech = fixture::speech();
    let count = 48_000 * 16;
    let render: Vec<f32> = (0..count)
        .map(|i| speech[i % speech.len()] * 0.65)
        .collect();
    let local: Vec<f32> = (0..count)
        .map(|i| {
            if (48_000 * 8..48_000 * 14).contains(&i) {
                speech[(i * 11 / 10 + 73123) % speech.len()] * 0.45
            } else {
                0.0
            }
        })
        .collect();
    for distorted in [false, true] {
        let speaker = |i: usize| {
            if distorted {
                render[i].clamp(-0.1, 0.1)
            } else {
                render[i]
            }
        };
        let echo: Vec<f32> = (0..count)
            .map(|i| {
                i.checked_sub(1920).map_or(0.0, |j| speaker(j) * 0.6)
                    + i.checked_sub(2640).map_or(0.0, |j| speaker(j) * 0.15)
            })
            .collect();
        for neural in [false, true] {
            for denoise in [false, true] {
                let settings = AudioSettings {
                    echo_cancellation: true,
                    neural_echo: neural,
                    neural_echo_model: neural.then(|| args[0].clone()),
                    noise_suppression: denoise,
                    noise_suppression_model: if denoise {
                        NoiseSuppressionModel::DeepFilterNet3
                    } else {
                        NoiseSuppressionModel::Sonora
                    },
                    ..Default::default()
                };
                let start = Instant::now();
                let mut dsp = Processing::new(&settings)?;
                let startup_ms = start.elapsed().as_secs_f64() * 1000.0;
                let mut times = Vec::new();
                let mut output = Vec::with_capacity(count);
                for offset in (0..count).step_by(480) {
                    let frame: &[f32; 480] = render[offset..offset + 480].try_into()?;
                    let mut mic = std::array::from_fn(|i| local[offset + i] + echo[offset + i]);
                    let start = Instant::now();
                    dsp.render(frame)?;
                    dsp.capture_frame(&mut mic, 40)?;
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                    if mic.iter().any(|v| !v.is_finite()) {
                        return Err("Non-finite processing output".into());
                    }
                    output.extend_from_slice(&mic);
                }
                let echo_gain = fixture::correlated_gain(&echo, &output, 48_000 * 4, 48_000 * 8);
                let local_gain = fixture::correlated_gain(&local, &output, 48_000 * 9, 48_000 * 14);
                times.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    serde_json::json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"neural":neural,"deep_filter":denoise,"clipped_speaker":distorted,"startup_ms":startup_ms,"mean_ms":times.iter().sum::<f64>()/times.len() as f64,"p99_ms":times[times.len()*99/100],"max_ms":times.last(),"missed_10ms":times.iter().filter(|&&v|v>=10.0).count(),"far_end_correlated_reduction_db":-20.0*echo_gain.max(1e-12).log10(),"double_talk_local_gain":local_gain,"scope":"synthetic room using synthetic speech; correlation is diagnostic, not perceptual quality"})
                );
            }
        }
    }
    Ok(())
}
