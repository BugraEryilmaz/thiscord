//! Re-run a diagnostic folder through current Rust DSP, without devices/network.
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter},
    path::Path,
};
use thiscord_frontend::audio::processing::Processing;
use thiscord_shared::audio::{AudioSettings, NoiseSuppressionModel};

fn wav(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != 48_000
        || spec.bits_per_sample != 32
        || spec.sample_format != hound::SampleFormat::Float
        || reader.len() > 48_000 * 60
    {
        return Err("Expected a bounded 48 kHz mono float diagnostic WAV".into());
    }
    let samples = reader
        .into_samples::<f32>()
        .collect::<Result<Vec<_>, _>>()?;
    if samples.iter().any(|s| !s.is_finite()) {
        return Err("Non-finite recorded sample".into());
    }
    Ok(samples)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    replay(&args)
}
fn replay(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 || !args.len().is_multiple_of(2) {
        return Err("Usage: audio_replay <recording-folder> <new-output.wav> [--neural-echo on|off] [--noise-suppression off|sonora|deep_filter_net3]".into());
    }
    let configure = |mut s: AudioSettings| -> Result<AudioSettings, Box<dyn std::error::Error>> {
        for pair in args[2..].as_chunks::<2>().0 {
            match (pair[0].as_str(), pair[1].as_str()) {
                ("--neural-echo", "on") => {
                    s.echo_cancellation = true;
                    s.neural_echo = true;
                }
                ("--neural-echo", "off") => s.neural_echo = false,
                ("--noise-suppression", "off") => s.noise_suppression = false,
                ("--noise-suppression", "sonora") => {
                    s.noise_suppression = true;
                    s.noise_suppression_model = NoiseSuppressionModel::Sonora;
                }
                ("--noise-suppression", "deep_filter_net3") => {
                    s.noise_suppression = true;
                    s.noise_suppression_model = NoiseSuppressionModel::DeepFilterNet3;
                }
                _ => return Err("Unknown replay option".into()),
            }
        }
        s.validate()?;
        Ok(s)
    };
    let folder = Path::new(&args[0]);
    let speaker = wav(&folder.join("speaker-output.wav"))?;
    let microphone = wav(&folder.join("microphone-input.wav"))?;
    let sent = wav(&folder.join("transmit-input.wav"))?;
    if speaker.len() != microphone.len() || microphone.len() != sent.len() {
        return Err("WAV timelines have different lengths".into());
    }
    let file = File::open(folder.join("timeline.jsonl"))?;
    if file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err("Timeline too large".into());
    }
    let mut lines = BufReader::new(file).lines();
    let header: Value = serde_json::from_str(&lines.next().ok_or("Missing timeline header")??)?;
    if header["type"] != "header"
        || header["schema"] != 1
        || header["rate"] != 48_000
        || header["channels"] != 1
    {
        return Err("Unsupported diagnostic format".into());
    }
    let settings = configure(serde_json::from_value(header["settings"].clone())?)?;
    let mut processing = Processing::new(&settings)?;
    let mut output = vec![0.0; microphone.len()];
    let mut processed = vec![0.0; microphone.len()];
    let mut complete = false;
    let mut capture_blocks = 0;
    for line in lines {
        let event: Value = serde_json::from_str(&line?)?;
        match event["type"].as_str() {
            Some("settings") => processing.settings(&configure(serde_json::from_value(
                event["settings"].clone(),
            )?)?)?,
            Some("end") => {
                if event["complete"] != true
                    || event["samples"].as_u64() != Some(output.len() as u64)
                {
                    return Err(
                        "Recording incomplete; do not use for regression comparisons".into(),
                    );
                }
                complete = true;
            }
            Some("block") => {
                if complete {
                    return Err("Events after recording end".into());
                }
                let offset = event["offset"].as_u64().ok_or("Missing offset")? as usize;
                let count = event["count"].as_u64().ok_or("Missing count")? as usize;
                let prefix = event["prefix"].as_u64().ok_or("Missing prefix")? as usize;
                if count > 480
                    || prefix > 480
                    || prefix + count > 480
                    || offset > output.len()
                    || count > output.len() - offset
                {
                    return Err("Invalid timeline bounds".into());
                }
                if count == 0 {
                    continue;
                }
                let range = offset..offset + count;
                let mut frame = [0.0; 480];
                match event["track"].as_str() {
                    Some("speaker") => {
                        if event["reset"] == true {
                            processing.reset();
                        }
                        frame[prefix..prefix + count].copy_from_slice(&speaker[range]);
                        processing.render(&frame)?;
                    }
                    Some("microphone") => {
                        if event["reset"] == true {
                            processing.reset();
                        }
                        frame[prefix..prefix + count].copy_from_slice(&microphone[range.clone()]);
                        let delay = event["delay_ms"]
                            .as_i64()
                            .filter(|d| (0..=500).contains(d))
                            .ok_or("Invalid capture delay")?;
                        processing.capture_frame(&mut frame, delay as i32)?;
                        processed[range].copy_from_slice(&frame[prefix..prefix + count]);
                        capture_blocks += 1;
                    }
                    Some("transmit") => {
                        if event["gate_open"] == true {
                            output[range.clone()].copy_from_slice(&processed[range]);
                        }
                    }
                    _ => return Err("Unknown recorded track".into()),
                }
            }
            _ => return Err("Unknown timeline event".into()),
        }
    }
    if !complete || capture_blocks == 0 {
        return Err("Recording is incomplete or has no microphone blocks".into());
    }
    let mut writer = hound::WavWriter::new(
        BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[1])?,
        ),
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )?;
    for sample in output {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    println!(
        "Replayed {capture_blocks} microphone blocks with recorded transmit gates. Skip the first 5–10 seconds when comparing adaptation. Output: {}",
        args[1]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn replay_preserves_alignment_and_recorded_gate_and_rejects_incomplete_input() {
        let folder = std::env::temp_dir().join(format!(
            "thiscord-replay-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        for name in [
            "speaker-output.wav",
            "microphone-input.wav",
            "transmit-input.wav",
        ] {
            let mut writer = hound::WavWriter::create(
                folder.join(name),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 48_000,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for _ in 0..960 {
                writer
                    .write_sample(if name == "microphone-input.wav" {
                        0.25_f32
                    } else {
                        0.0
                    })
                    .unwrap();
            }
            writer.finalize().unwrap();
        }
        let mut timeline = format!(
            "{}\n",
            json!({"type":"header","schema":1,"rate":48000,"channels":1,"settings":AudioSettings::default()})
        );
        for offset in [0, 480] {
            for track in ["speaker", "microphone", "transmit"] {
                timeline.push_str(&format!("{}\n",json!({"type":"block","track":track,"offset":offset,"count":480,"prefix":0,"gap":false,"delay_ms":0,"gate_open":offset==480})));
            }
        }
        let meta = folder.join("timeline.jsonl");
        std::fs::write(
            &meta,
            format!(
                "{timeline}{}\n",
                json!({"type":"end","complete":true,"samples":960})
            ),
        )
        .unwrap();
        let output = folder.join("replay.wav");
        let args = vec![
            folder.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
        ];
        replay(&args).unwrap();
        let samples = wav(&output).unwrap();
        assert_eq!(&samples[..480], &[0.0; 480]);
        assert_eq!(&samples[480..], &[0.25; 480]);
        assert!(
            replay(&args).is_err(),
            "must not overwrite existing audio files"
        );
        std::fs::write(meta, timeline).unwrap();
        assert!(
            replay(&[
                args[0].clone(),
                folder.join("incomplete.wav").to_string_lossy().into_owned()
            ])
            .is_err()
        );
        assert!(!folder.join("incomplete.wav").exists());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
