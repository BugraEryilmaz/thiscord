//! Untrusted uploads become bounded, loudness-normalized mono 48 kHz PCM.
//! Runs on a blocking worker without a database connection checked out.
use crate::auth::Failure;
use rubato::{FftFixedInOut, Resampler};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, DecoderOptions},
    errors::Error,
    formats::FormatOptions,
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
};
use thiscord_shared::soundboard::{MAX_SAMPLES, MAX_UPLOAD_BYTES, SAMPLE_RATE};

/// Source audio decoded before trimming; bounds CPU and memory for long files.
const MAX_SOURCE_SECONDS: usize = 60;
const MIN_SAMPLES: usize = SAMPLE_RATE as usize / 20;
const SILENCE: f32 = 0.001;
const TARGET_RMS: f32 = 0.125;
const PEAK_LIMIT: f32 = 0.89;
const MAX_GAIN: f32 = 8.0;
const FADE: usize = SAMPLE_RATE as usize / 100;

fn invalid() -> Failure {
    Failure::Invalid("Choose a WAV, MP3, Ogg Vorbis or FLAC sound up to 1 MiB")
}

pub(super) struct Clip {
    pub samples: Vec<i16>,
    pub duration_ms: u32,
}

pub(super) fn normalize(bytes: Vec<u8>) -> Result<Clip, Failure> {
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(invalid());
    }
    prepare(bytes)
}

/// The upload pipeline without the upload size limit, for bundled assets.
/// Decoding is still bounded by `MAX_SOURCE_SECONDS`.
pub(super) fn prepare(bytes: Vec<u8>) -> Result<Clip, Failure> {
    let (mono, rate) = decode(bytes)?;
    let start = mono.iter().position(|s| s.abs() > SILENCE);
    let end = mono.iter().rposition(|s| s.abs() > SILENCE);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(Failure::Invalid("The sound is silent"));
    };
    // Trim at the source rate, keeping a little more than the output allows.
    let limit =
        (MAX_SAMPLES as u64 * u64::from(rate)).div_ceil(u64::from(SAMPLE_RATE)) as usize + 1;
    let source = &mono[start..=end.min(start + limit)];
    let mut samples = resample(source, rate)?;
    let truncated = samples.len() > MAX_SAMPLES;
    samples.truncate(MAX_SAMPLES);
    if samples.len() < MIN_SAMPLES {
        return Err(Failure::Invalid("The sound must be at least 50 ms long"));
    }
    if truncated {
        let length = samples.len();
        for (index, sample) in samples[length - FADE..].iter_mut().enumerate() {
            *sample *= 1.0 - (index + 1) as f32 / FADE as f32;
        }
    }
    level(samples)
}

/// Loudness target shared by every clip clients receive.
fn level(samples: Vec<f32>) -> Result<Clip, Failure> {
    let energy = samples
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>();
    let rms = (energy / samples.len() as f64).sqrt() as f32;
    let peak = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    if !rms.is_finite() || !peak.is_finite() || peak <= SILENCE {
        return Err(Failure::Invalid("The sound is silent"));
    }
    let gain = (TARGET_RMS / rms).min(PEAK_LIMIT / peak).min(MAX_GAIN);
    let samples: Vec<i16> = samples
        .into_iter()
        .map(|s| ((s * gain).clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16)
        .collect();
    Ok(Clip {
        duration_ms: (samples.len() as u64 * 1000 / u64::from(SAMPLE_RATE)) as u32,
        samples,
    })
}

fn decode(bytes: Vec<u8>) -> Result<(Vec<f32>, u32), Failure> {
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes)),
        MediaSourceStreamOptions::default(),
    );
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| invalid())?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .filter(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(invalid)?;
    let track_id = track.id;
    let rate = track
        .codec_params
        .sample_rate
        .filter(|rate| (8_000..=192_000).contains(rate))
        .ok_or_else(invalid)?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| invalid())?;
    let max_frames = rate as usize * MAX_SOURCE_SECONDS;
    let mut mono = Vec::new();
    while mono.len() < max_frames {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(_) if !mono.is_empty() => break,
            Err(_) => return Err(invalid()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A corrupt frame is skipped, as players do; the input size bounds retries.
            Err(Error::DecodeError(_)) => continue,
            Err(_) => return Err(invalid()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        if spec.rate != rate || !(1..=8).contains(&channels) {
            return Err(invalid());
        }
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks_exact(channels) {
            let sample = frame.iter().sum::<f32>() / channels as f32;
            mono.push(if sample.is_finite() { sample } else { 0.0 });
        }
    }
    mono.truncate(max_frames);
    if mono.is_empty() {
        return Err(invalid());
    }
    Ok((mono, rate))
}

fn resample(input: &[f32], rate: u32) -> Result<Vec<f32>, Failure> {
    if rate == SAMPLE_RATE {
        return Ok(input.to_vec());
    }
    let mut resampler = FftFixedInOut::<f32>::new(rate as usize, SAMPLE_RATE as usize, 1024, 1)
        .map_err(|_| invalid())?;
    let delay = resampler.output_delay();
    let expected = (input.len() as u64 * u64::from(SAMPLE_RATE) / u64::from(rate)) as usize;
    let mut output = Vec::with_capacity(expected + delay + resampler.output_frames_max());
    let mut position = 0;
    let mut chunk = Vec::new();
    while output.len() < expected + delay {
        let needed = resampler.input_frames_next();
        chunk.clear();
        chunk.resize(needed, 0.0);
        let end = (position + needed).min(input.len());
        if position < end {
            chunk[..end - position].copy_from_slice(&input[position..end]);
        }
        position += needed;
        let produced = resampler
            .process(&[&chunk], None)
            .map_err(|_| Failure::Unavailable)?;
        output.extend_from_slice(&produced[0]);
    }
    Ok(output[delay..delay + expected].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, seconds: f32, amplitude: f32) -> Vec<u8> {
        let frames = (rate as f32 * seconds) as usize;
        let mut data = Vec::new();
        for n in 0..frames {
            let value = (n as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * amplitude;
            for _ in 0..channels {
                data.extend_from_slice(&((value * 32767.0) as i16).to_le_bytes());
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn resamples_downmixes_and_normalizes_loudness() {
        let clip = normalize(wav(44_100, 2, 1.0, 0.05)).unwrap();
        assert!(
            (clip.duration_ms as i64 - 1000).abs() <= 2,
            "{}",
            clip.duration_ms
        );
        assert!(clip.samples.len().abs_diff(SAMPLE_RATE as usize) <= 48);
        // A sine at the target RMS peaks at TARGET_RMS * sqrt(2).
        let peak = clip.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((5_400..=6_200).contains(&peak), "{peak}");
        // Loud input is limited by the peak ceiling, quiet input by the maximum gain.
        let loud = normalize(wav(48_000, 1, 1.0, 1.0)).unwrap();
        let peak = loud.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak <= (PEAK_LIMIT * 32767.0) as u16 + 1, "{peak}");
        let quiet = normalize(wav(48_000, 1, 1.0, 0.005)).unwrap();
        let peak = quiet
            .samples
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!(peak < (0.005 * MAX_GAIN * 32767.0) as u16 + 50, "{peak}");
    }

    #[test]
    fn trims_leading_silence_and_bounds_duration_with_fade() {
        let mut long = wav(48_000, 1, 0.5, 0.0);
        let tone = wav(48_000, 1, 7.0, 0.5);
        // Splice: header of the combined file plus both data chunks.
        let silence = long.split_off(44);
        let mut data = silence;
        data.extend_from_slice(&tone[44..]);
        let mut file = wav(48_000, 1, 0.0, 0.0);
        file[4..8].copy_from_slice(&(36 + data.len() as u32).to_le_bytes());
        file[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
        file.extend_from_slice(&data);
        let clip = normalize(file).unwrap();
        assert_eq!(clip.samples.len(), MAX_SAMPLES);
        assert_eq!(clip.duration_ms, 5_000);
        assert!(clip.samples[..100].iter().any(|s| s.unsigned_abs() > 1000));
        assert!(clip.samples.last().unwrap().unsigned_abs() < 100);
    }

    #[test]
    fn rejects_garbage_silence_and_oversized_input() {
        assert!(matches!(normalize(vec![]), Err(Failure::Invalid(_))));
        assert!(matches!(
            normalize(b"not audio at all".repeat(100)),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            normalize(wav(48_000, 1, 1.0, 0.0)),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            normalize(wav(48_000, 1, 0.01, 0.5)),
            Err(Failure::Invalid(_))
        ));
        assert!(matches!(
            normalize(vec![0; MAX_UPLOAD_BYTES + 1]),
            Err(Failure::Invalid(_))
        ));
    }
}
