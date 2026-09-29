use super::frames;
use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{
        AdaptiveDigital, EchoCanceller, GainController2, NoiseSuppression, NoiseSuppressionLevel,
    },
};
use std::time::Instant;
use thiscord_shared::audio::AudioSettings;
pub struct Processing {
    apm: AudioProcessing,
    config: Config,
    render_at: Option<Instant>,
    pub resets: u64,
}
fn config(s: &AudioSettings) -> Config {
    Config {
        echo_canceller: s.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: s.noise_suppression.then_some(NoiseSuppression {
            level: NoiseSuppressionLevel::High,
            ..Default::default()
        }),
        // The library default only enables a limiter, not adaptive gain.
        // Start at unity and bound amplification of residual room noise.
        gain_controller2: s.automatic_gain.then_some(GainController2 {
            adaptive_digital: Some(AdaptiveDigital {
                initial_gain_db: 0.0,
                max_gain_db: 20.0,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}
impl Processing {
    pub fn new(s: &AudioSettings) -> Self {
        Self::with_config(config(s))
    }
    fn with_config(config: Config) -> Self {
        Self {
            apm: AudioProcessing::builder()
                .config(config.clone())
                .capture_config(StreamConfig::new(48_000, 1))
                .render_config(StreamConfig::new(48_000, 1))
                .build(),
            config,
            render_at: None,
            resets: 0,
        }
    }
    pub fn reset(&mut self) {
        let resets = self.resets + 1;
        *self = Self::with_config(self.config.clone());
        self.resets = resets;
    }
    pub fn settings(&mut self, s: &AudioSettings) {
        let config = config(s);
        if config.echo_canceller != self.config.echo_canceller
            || config.noise_suppression != self.config.noise_suppression
            || config.gain_controller2 != self.config.gain_controller2
        {
            self.apm.apply_config(config.clone());
            self.config = config;
        }
    }
    pub fn render(&mut self, frame: &[f32; 480]) -> Result<(), String> {
        let mut out = [0.0; 480];
        self.apm
            .process_render_f32(&[frame], &mut [&mut out])
            .map_err(|e| e.to_string())
    }
    pub(super) fn render_queued(&mut self, queue: &mut frames::Reader) -> Result<(), String> {
        for _ in 0..frames::BLOCKS {
            let Some((frame, gap)) = queue.pop() else {
                break;
            };
            if gap {
                self.reset();
            }
            self.render(&frame.samples)?;
            self.render_at = Some(frame.at);
        }
        Ok(())
    }
    pub(super) fn capture_queued(
        &mut self,
        queue: &mut frames::Reader,
    ) -> Result<Option<[f32; 960]>, String> {
        if queue.len() < 2 {
            return Ok(None);
        }
        let (first, first_gap) = queue.pop().expect("two complete frames");
        let (second, second_gap) = queue.pop().expect("two complete frames");
        if first_gap || second_gap {
            self.reset();
        }
        let mut pcm = [0.0; 960];
        pcm[..480].copy_from_slice(&first.samples);
        pcm[480..].copy_from_slice(&second.samples);
        for (frame, at) in pcm
            .as_chunks_mut::<480>()
            .0
            .iter_mut()
            .zip([first.at, second.at])
        {
            self.capture_frame(frame, frames::delay_ms(self.render_at, at))?;
        }
        Ok(Some(pcm))
    }
    fn capture_frame(&mut self, frame: &mut [f32; 480], delay_ms: i32) -> Result<(), String> {
        // Device timestamps account for buffering; AEC3 estimates the acoustic
        // path, including room reflections, from the actual playback reference.
        self.apm
            .set_stream_delay_ms(delay_ms.clamp(0, 500))
            .map_err(|e| e.to_string())?;
        let mut out = [0.0; 480];
        self.apm
            .process_capture_f32(&[frame], &mut [&mut out])
            .map_err(|e| e.to_string())?;
        frame.copy_from_slice(&out);
        Ok(())
    }
    #[cfg(test)]
    pub fn capture(&mut self, pcm: &mut [f32; 960], delay_ms: i32) -> Result<(), String> {
        for (index, frame) in pcm.as_chunks_mut::<480>().0.iter_mut().enumerate() {
            self.capture_frame(frame, delay_ms - index as i32 * 10)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn noise(state: &mut u32) -> f32 {
        *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (*state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
    }

    fn energy(samples: &[f32]) -> f64 {
        samples.iter().map(|&v| f64::from(v).powi(2)).sum()
    }

    #[test]
    fn stationary_noise_is_attenuated() {
        let mut p = Processing::new(&AudioSettings {
            noise_suppression: true,
            ..Default::default()
        });
        let mut state = 42;
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..400 {
            let mut pcm = std::array::from_fn(|_| noise(&mut state) * 0.03);
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame >= 200 {
                before += input;
                after += energy(&pcm);
            }
        }
        let reduction = 10.0 * (before / after).log10();
        eprintln!("Stationary noise reduction: {reduction:.1} dB");
        assert!(reduction > 17.0, "noise reduction: {reduction:.1} dB");
    }

    #[test]
    fn echo_cancellation_handles_batched_device_callbacks() {
        for batch in [960, 4800] {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                ..Default::default()
            });
            let (mut render, mut references) = frames::queue();
            let (mut capture, mut captures) = frames::queue();
            let mut state = 456;
            let history: Vec<_> = (0..48_000 * 12).map(|_| noise(&mut state) * 0.2).collect();
            let start = Instant::now();
            let (mut before, mut after, mut delivered) = (0.0, 0.0, 0);
            for offset in (0..history.len()).step_by(batch) {
                let at = start + frames::sample_duration(offset);
                // Independent devices: playback is 50 ms ahead, captured audio
                // is 30 ms old. Add 5 ms propagation and a 15 ms room reflection.
                render.begin(at + std::time::Duration::from_millis(50));
                capture.begin(at - std::time::Duration::from_millis(30));
                for n in offset..offset + batch {
                    assert_eq!(render.push(history[n]), 0);
                    let sample = n.checked_sub(4080).map_or(0.0, |j| history[j] * 0.6)
                        + n.checked_sub(4800).map_or(0.0, |j| history[j] * 0.15);
                    assert_eq!(capture.push(sample), 0);
                    if offset >= 48_000 * 8 {
                        before += f64::from(sample).powi(2);
                    }
                }
                p.render_queued(&mut references).unwrap();
                while let Some(pcm) = p.capture_queued(&mut captures).unwrap() {
                    delivered += pcm.len();
                    if offset >= 48_000 * 8 {
                        after += energy(&pcm);
                    }
                }
            }
            assert_eq!(delivered, history.len());
            assert_eq!(p.resets, 0);
            let reduction = 10.0 * (before / after).log10();
            eprintln!(
                "Echo reduction, {} ms callbacks: {reduction:.1} dB",
                batch / 48
            );
            assert!(reduction > 20.0, "batch {batch}: {reduction:.1} dB");
        }
    }

    #[test]
    fn echo_cancellation_preserves_near_end_audio_without_playback() {
        let mut p = Processing::new(&AudioSettings {
            echo_cancellation: true,
            ..Default::default()
        });
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..300 {
            // Voiced harmonics, independent of the silent speaker reference.
            let mut pcm = std::array::from_fn(|i| {
                let t = (frame * 960 + i) as f32 / 48_000.0;
                [220.0, 440.0, 880.0]
                    .into_iter()
                    .map(|hz| (t * hz * std::f32::consts::TAU).sin() * 0.03)
                    .sum()
            });
            p.render(&[0.0; 480]).unwrap();
            p.render(&[0.0; 480]).unwrap();
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame > 200 {
                before += input;
                after += energy(&pcm);
            }
        }
        assert!(
            after / before > 0.5,
            "near-end speech must not be muted: {}",
            after / before
        );
    }

    #[test]
    fn disabling_suppression_live_restores_unprocessed_audio() {
        let mut p = Processing::new(&AudioSettings {
            noise_suppression: true,
            ..Default::default()
        });
        for _ in 0..50 {
            p.capture(&mut [0.01; 960], 0).unwrap();
        }
        p.settings(&AudioSettings::default());
        let mut state = 789;
        let input = std::array::from_fn(|_| noise(&mut state) * 0.1);
        let mut output = input;
        p.capture(&mut output, 0).unwrap();
        assert_eq!(input, output);
    }

    #[test]
    fn capture_and_reference_overflow_restart_adaptation_once_per_gap() {
        for render_gap in [false, true] {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                ..Default::default()
            });
            let (mut writer, mut reader) = frames::queue();
            for _ in 0..(frames::BLOCKS + 2) * frames::SAMPLES {
                writer.push(0.0);
            }
            if render_gap {
                p.render_queued(&mut reader).unwrap();
            } else {
                while p.capture_queued(&mut reader).unwrap().is_some() {}
            }
            assert_eq!(p.resets, 0);
            for _ in 0..960 {
                writer.push(0.0);
            }
            if render_gap {
                p.render_queued(&mut reader).unwrap();
            } else {
                assert!(p.capture_queued(&mut reader).unwrap().is_some());
            }
            assert_eq!(p.resets, 1);
        }
    }

    #[test]
    fn delayed_speaker_echo_is_attenuated() {
        let mut p = Processing::new(&AudioSettings {
            echo_cancellation: true,
            ..Default::default()
        });
        let mut state = 123;
        let mut history = vec![0.0; 48_000 * 12];
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..600 {
            let offset = frame * 960;
            for sample in &mut history[offset..offset + 960] {
                *sample = noise(&mut state) * 0.2;
            }
            for render in history[offset..offset + 960].as_chunks::<480>().0 {
                p.render(render).unwrap();
            }
            let mut pcm = std::array::from_fn(|i| {
                let n = offset + i;
                // A 40 ms direct path plus a quieter room reflection at 55 ms.
                n.checked_sub(1920).map_or(0.0, |j| history[j] * 0.6)
                    + n.checked_sub(2640).map_or(0.0, |j| history[j] * 0.15)
            });
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame >= 400 {
                before += input;
                after += energy(&pcm);
            }
        }
        let reduction = 10.0 * (before / after).log10();
        eprintln!("Delayed echo reduction: {reduction:.1} dB");
        assert!(reduction > 20.0, "echo reduction: {reduction:.1} dB");
    }
    #[test]
    fn processing_stays_finite_with_render_reference() {
        let s = AudioSettings {
            noise_suppression: true,
            automatic_gain: true,
            echo_cancellation: true,
            ..Default::default()
        };
        let mut p = Processing::new(&s);
        for _ in 0..20 {
            p.render(&[0.0; 480]).unwrap();
            let mut input = [0.001; 960];
            p.capture(&mut input, 40).unwrap();
            assert!(input.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        }
    }
}
