//! Bundled synthetic speech and deterministic acoustic backgrounds, before DSP.
use thiscord_shared::voice_bots::MicrophoneLoop as Kind;

#[derive(Clone, Copy)]
enum Background {
    Clean,
    Fan,
    Keyboard,
    Cafe,
    Street,
    Rain,
}
use Background::*;

const ASSETS: [(Kind, &[u8], Background); 13] = [
    (
        Kind::EnglishClean,
        include_bytes!("../../../assets/voice-bots/english_clean.wav"),
        Clean,
    ),
    (
        Kind::EnglishFan,
        include_bytes!("../../../assets/voice-bots/english_fan.wav"),
        Fan,
    ),
    (
        Kind::EnglishKeyboard,
        include_bytes!("../../../assets/voice-bots/english_keyboard.wav"),
        Keyboard,
    ),
    (
        Kind::EnglishCafe,
        include_bytes!("../../../assets/voice-bots/english_cafe.wav"),
        Cafe,
    ),
    (
        Kind::EnglishStreet,
        include_bytes!("../../../assets/voice-bots/english_street.wav"),
        Street,
    ),
    (
        Kind::EnglishRain,
        include_bytes!("../../../assets/voice-bots/english_rain.wav"),
        Rain,
    ),
    (
        Kind::TurkishClean,
        include_bytes!("../../../assets/voice-bots/turkish_clean.wav"),
        Clean,
    ),
    (
        Kind::TurkishFan,
        include_bytes!("../../../assets/voice-bots/turkish_fan.wav"),
        Fan,
    ),
    (
        Kind::TurkishKeyboard,
        include_bytes!("../../../assets/voice-bots/turkish_keyboard.wav"),
        Keyboard,
    ),
    (
        Kind::TurkishCafe,
        include_bytes!("../../../assets/voice-bots/turkish_cafe.wav"),
        Cafe,
    ),
    (
        Kind::TurkishStreet,
        include_bytes!("../../../assets/voice-bots/turkish_street.wav"),
        Street,
    ),
    (
        Kind::TurkishRain,
        include_bytes!("../../../assets/voice-bots/turkish_rain.wav"),
        Rain,
    ),
    (
        Kind::Speech,
        include_bytes!("../../../tests/fixtures/speech.wav"),
        Clean,
    ),
];

pub(super) struct Library {
    clips: Vec<Vec<f32>>,
}
impl Library {
    /// Decode once before starting the callback thread. No I/O, allocation or
    /// synthesis service is needed while sampling or changing loop selections.
    pub fn load() -> Result<Self, String> {
        let clips = ASSETS
            .iter()
            .map(|(_, bytes, _)| {
                let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
                    .map_err(|_| "Invalid bot speech WAV")?;
                let spec = reader.spec();
                if spec.channels != 1
                    || spec.sample_rate != 48_000
                    || spec.bits_per_sample != 16
                    || spec.sample_format != hound::SampleFormat::Int
                    || reader.duration() == 0
                    || reader.duration() > 48_000 * 30
                {
                    return Err("Invalid bot speech format");
                }
                let mut samples = reader
                    .samples::<i16>()
                    .map(|s| s.map(|v| v as f32 / 32768.0))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "Invalid bot speech samples")?;
                let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                if peak < 0.001 {
                    return Err("Empty bot speech");
                }
                let len = samples.len();
                for (i, sample) in samples.iter_mut().enumerate() {
                    let fade = (i.min(len - 1 - i) as f32 / 240.0).min(1.0);
                    *sample *= 0.25 / peak * fade;
                }
                Ok(samples)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(str::to_owned)?;
        Ok(Self { clips })
    }

    fn voice(&self, index: usize, sample: u64) -> f32 {
        let clip = &self.clips[index];
        // One second with no foreground voice also exercises release and VAD.
        clip.get((sample % (clip.len() as u64 + 48_000)) as usize)
            .copied()
            .unwrap_or(0.0)
    }

    pub fn sample(&self, kind: Kind, sample: u64) -> Option<f32> {
        let index = ASSETS.iter().position(|(k, _, _)| *k == kind)?;
        let time = (sample % (48_000 * 60)) as f32 / 48_000.0;
        let background = match ASSETS[index].2 {
            Clean => 0.0,
            Fan => low_noise(sample, 240) * 0.035 + sine(time * 90.0) * 0.006,
            Keyboard => {
                let key = sample / 7200;
                let offset = sample % 7200;
                // Irregular bursts and pauses, with short damped key impacts.
                if noise(key) > -0.3 && offset < 1600 {
                    noise(sample) * (-(offset as f32) / 260.0).exp() * 0.13
                } else {
                    0.0
                }
            }
            Cafe => {
                let base = if index < 6 { 0 } else { 6 };
                self.voice(base, sample + 96_000) * 0.16
                    + self.voice(base + 1, sample + 211_000) * 0.13
                    + low_noise(sample, 24) * 0.008
            }
            Street => {
                let passing = 0.5 + 0.5 * sine((sample % 336_000) as f32 / 336_000.0);
                low_noise(sample, 120) * (0.015 + passing * 0.055)
                    + sine(time * 65.0) * passing * 0.008
            }
            Rain => noise(sample) * 0.025 + low_noise(sample, 20) * 0.02,
        };
        // Background continues through pauses; start it gently on first join.
        let fade = (sample as f32 / 480.0).min(1.0);
        Some(self.voice(index, sample) + background * fade)
    }
}
fn sine(cycles: f32) -> f32 {
    (std::f32::consts::TAU * cycles).sin()
}
fn noise(sample: u64) -> f32 {
    let mut value = sample.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((value ^ (value >> 31)) >> 40) as f32 / 8_388_608.0 - 1.0
}
fn low_noise(sample: u64, width: u64) -> f32 {
    let phase = (sample % width) as f32 / width as f32;
    noise(sample / width) * (1.0 - phase) + noise(sample / width + 1) * phase
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn twelve_distinct_speech_assets_are_valid_and_bounded_with_continuous_backgrounds() {
        let library = Library::load().unwrap();
        let mut fingerprints = std::collections::HashSet::new();
        for (index, (kind, bytes, background)) in ASSETS.iter().enumerate() {
            assert!(library.clips[index].len() > 48_000 * 3);
            assert!(fingerprints.insert(*bytes), "duplicate speech asset");
            assert!(Kind::ALL.contains(kind));
            let end = library.clips[index].len() as u64;
            let mut energy = 0.0f64;
            for n in 0..end + 48_000 {
                let value = library.sample(*kind, n).unwrap();
                assert!(value.is_finite() && value.abs() < 0.5);
                if n >= end {
                    energy += f64::from(value).powi(2);
                }
            }
            match background {
                Clean => assert_eq!(energy, 0.0),
                _ => assert!(energy > 0.001, "noise must persist between sentences"),
            }
            assert_eq!(library.voice(index, end), 0.0);
            assert_eq!(library.voice(index, end + 48_000), library.voice(index, 0));
        }
        for kind in Kind::ALL {
            if !matches!(kind, Kind::Chimes | Kind::Pulse | Kind::Melody) {
                assert!(library.sample(kind, 48_000).is_some());
            }
        }
        assert!(library.sample(Kind::Chimes, 0).is_none());
    }
}
