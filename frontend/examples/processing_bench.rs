//! Offline only: no devices, network, microphone recording or model downloads.
#[path = "../tests/support/audio_fixture.rs"]
mod fixture;
use std::{hint::black_box, time::Instant};
use thiscord_frontend::audio::processing::Processing;
use thiscord_shared::audio::{AudioSettings, NoiseSuppressionModel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let frames = args
        .first()
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(500);
    if !(100..=100_000).contains(&frames) || args.len() > 2 {
        return Err("Usage: processing_bench [frames:100..100000] [48k-mono-PCM16.wav]".into());
    }
    let speech = if let Some(path) = args.get(1) {
        let mut reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        if spec.sample_rate != 48_000
            || spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err("Fixture must be 48 kHz mono PCM16 WAV".into());
        }
        // Bound user-supplied files to one minute. Never emit samples or paths.
        reader
            .samples::<i16>()
            .take(48_000 * 60)
            .map(|s| s.map(|s| s as f32 / 32768.0))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        fixture::speech()
    };
    if speech.is_empty() {
        return Err("Empty speech fixture".into());
    }
    println!(
        "{}",
        serde_json::json!({"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"debug_assertions":cfg!(debug_assertions),"frames":frames,"sample_rate":48000,"hop_samples":480,"scope":"worker DSP only; excludes devices, mixer, codecs and network"})
    );
    for (name, model, enabled, combined) in [
        ("bypass", NoiseSuppressionModel::Sonora, false, false),
        ("sonora", NoiseSuppressionModel::Sonora, true, false),
        (
            "deep_filter_net3",
            NoiseSuppressionModel::DeepFilterNet3,
            true,
            false,
        ),
        (
            "aec_deep_filter_net3_gain",
            NoiseSuppressionModel::DeepFilterNet3,
            true,
            true,
        ),
    ] {
        let start = Instant::now();
        let mut processor = Processing::new(&AudioSettings {
            noise_suppression: enabled,
            noise_suppression_model: model,
            echo_cancellation: combined,
            automatic_gain: combined,
            ..Default::default()
        })?;
        let initialization_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        processor.reset();
        let reset_ms = start.elapsed().as_secs_f64() * 1000.0;
        for kind in ["speech_noise", "noise", "silence"] {
            processor.reset();
            let mut rng = 42;
            let mut times = Vec::with_capacity(frames);
            let mut input_energy = 0.0;
            let mut output_energy = 0.0;
            for n in 0..frames + 100 {
                let mut block = std::array::from_fn(|i| match kind {
                    "speech_noise" => {
                        speech[(n * 480 + i) % speech.len()] * 0.7 + fixture::noise(&mut rng)
                    }
                    "noise" => fixture::noise(&mut rng),
                    _ => 0.0,
                });
                let before = fixture::energy(&block);
                // Independent far-end signal exercises the AEC render path too.
                let reference =
                    std::array::from_fn(|i| ((n * 480 + i) as f32 * 0.028).sin() * 0.04);
                let at = Instant::now();
                if combined {
                    processor.render(black_box(&reference))?;
                }
                processor.capture_frame(black_box(&mut block), 40)?;
                let elapsed = at.elapsed().as_secs_f64() * 1000.0;
                black_box(&block);
                if n >= 100 {
                    times.push(elapsed);
                    input_energy += before;
                    output_energy += fixture::energy(&block);
                }
            }
            let total: f64 = times.iter().sum();
            let misses = times.iter().filter(|&&t| t > 10.0).count();
            times.sort_by(f64::total_cmp);
            let percentile = |p: f64| times[((frames - 1) as f64 * p).ceil() as usize];
            println!(
                "{}",
                serde_json::json!({"pipeline":name,"fixture":kind,"initialization_ms":initialization_ms,"reset_ms":reset_ms,"delay_samples":processor.enhancement_delay_samples(),"mean_ms":total/frames as f64,"p50_ms":percentile(0.5),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"max_ms":times[frames-1],"rtf":total/(frames as f64*10.0),"deadline_misses":misses,"input_energy":input_energy,"output_energy":output_energy})
            );
        }
        // Quality result is separate from timings and aligns known model delay.
        processor.reset();
        let mut rng = 42;
        let max_delay = 4800;
        let count = speech.len().min(48_000 * 4) / 480 * 480;
        let mut output = Vec::with_capacity(count + max_delay + 480);
        let noisy: Vec<_> = speech[..count]
            .iter()
            .map(|s| s * 0.7 + fixture::noise(&mut rng))
            .collect();
        for offset in (0..count + max_delay).step_by(480) {
            let mut block = std::array::from_fn(|i| noisy.get(offset + i).copied().unwrap_or(0.0));
            if combined {
                processor.render(&[0.0; 480])?;
            }
            processor.capture_frame(&mut block, 0)?;
            output.extend_from_slice(&block);
        }
        let clean: Vec<_> = speech[..count].iter().map(|s| s * 0.7).collect();
        let delay = fixture::best_delay(&clean, &output, max_delay);
        println!(
            "{}",
            serde_json::json!({"pipeline":name,"measured_alignment_samples":delay,"aligned_input_snr_db":fixture::snr(&clean,&noisy),"aligned_output_snr_db":fixture::snr(&clean,&output[delay..delay+count]),"quality_note":"synthetic speech + seeded white noise; not a perceptual or speaker-isolation score"})
        );
    }
    Ok(())
}
