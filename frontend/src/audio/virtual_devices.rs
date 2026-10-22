//! Headless device callbacks feeding the ordinary capture, DSP, jitter and mixer path.
use super::*;
use std::sync::{OnceLock, atomic::AtomicU32};
use thiscord_shared::voice_bots::{MicrophoneLoop, Scenario, Settings};

static SOURCE: OnceLock<Source> = OnceLock::new();
struct Source {
    configuration: AtomicU32,
    peak: AtomicU32,
}
pub fn initialize(settings: &Settings) {
    SOURCE.get_or_init(|| Source {
        configuration: 0.into(),
        peak: 0.into(),
    });
    update(settings);
}
pub fn enabled() -> bool {
    SOURCE.get().is_some()
}
pub fn update(settings: &Settings) {
    if let Some(source) = SOURCE.get() {
        let input = MicrophoneLoop::ALL
            .iter()
            .position(|v| *v == settings.microphone_loop)
            .unwrap();
        let scenario = Scenario::ALL
            .iter()
            .position(|v| *v == settings.scenario)
            .unwrap();
        source
            .configuration
            .store((input | scenario << 8) as u32, Ordering::Release);
    }
}
pub fn peak() -> f32 {
    SOURCE
        .get()
        .map(|s| f32::from_bits(s.peak.load(Ordering::Relaxed)))
        .unwrap_or(0.0)
}
pub fn devices() -> Vec<AudioDevice> {
    vec![
        AudioDevice {
            id: "virtual-microphone".into(),
            name: "Test microphone loop".into(),
            input: true,
        },
        AudioDevice {
            id: "virtual-speakers".into(),
            name: "Silent speakers with echo reference".into(),
            input: false,
        },
    ]
}
struct Streams {
    playing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl DeviceStreams for Streams {
    fn play(&self) -> Result<(), String> {
        self.playing.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for Streams {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub(super) fn prepare(settings: &AudioSettings, microphone: bool) -> Result<Devices, String> {
    thiscord_shared::voice_bots::validate_audio(settings)?;
    let control = Arc::new(Controls::default());
    let health = Arc::new(StreamHealth::default());
    apply(&control, settings);
    let (writers, mix) = mixer(control.clone());
    let (reference_writer, reference) = frames::queue();
    let mut mix = mix.with_reference(reference_writer);
    let (mut capture_writer, capture) = frames::queue();
    let playing = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (active, stopping, counters, activity) = (
        playing.clone(),
        stop.clone(),
        control.clone(),
        health.clone(),
    );
    let speech = hound::WavReader::new(std::io::Cursor::new(include_bytes!(
        "../../tests/fixtures/speech.wav"
    )))
    .map_err(|_| "Invalid speech fixture")?
    .samples::<i16>()
    .map(|s| s.map(|s| s as f32 / 32768.0))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| "Invalid speech fixture")?;
    let thread = thread::Builder::new()
        .name("thiscord-virtual-audio".into())
        .spawn(move || {
            let source = SOURCE.get().unwrap();
            let mut position = 0u64;
            let mut noise = 19u32;
            let mut rendered = [0.0f32; 480];
            let mut echo = [0.0f32; 3840]; // 80 ms acoustic delay, independent of the network.
            let mut cursor = 0;
            let mut next = Instant::now();
            while !stopping.load(Ordering::Acquire) && !activity.disabled() {
                if active.load(Ordering::Acquire) {
                    let now = Instant::now();
                    let configuration = source.configuration.load(Ordering::Acquire);
                    let kind = MicrophoneLoop::ALL[(configuration & 255) as usize];
                    let scenario = Scenario::ALL[(configuration >> 8) as usize];
                    activity.callback(false);
                    mix.reference_time(now);
                    mix.render(&mut rendered, 1);
                    source.peak.store(
                        rendered
                            .iter()
                            .map(|s| s.abs())
                            .fold(0.0, f32::max)
                            .to_bits(),
                        Ordering::Relaxed,
                    );
                    if microphone {
                        activity.callback(true);
                        capture_writer.begin_capture(now);
                        let (mut peak, mut dropped, mut clipped) = (0.0f32, 0, 0);
                        for rendered in rendered {
                            let mut sample = match scenario {
                                Scenario::Echo | Scenario::Silence => 0.0,
                                _ => loop_sample(kind, position, &speech),
                            };
                            if scenario == Scenario::Quiet {
                                sample *= 0.08;
                            }
                            if scenario == Scenario::Noise {
                                noise = noise.wrapping_mul(1664525).wrapping_add(1013904223);
                                sample +=
                                    (noise as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32 * 0.035;
                            }
                            if matches!(scenario, Scenario::Echo | Scenario::DoubleTalk) {
                                sample += echo[cursor] * 0.5;
                            }
                            echo[cursor] = rendered;
                            cursor = (cursor + 1) % echo.len();
                            position += 1;
                            sample = sample.clamp(-1.0, 1.0);
                            peak = peak.max(sample.abs());
                            clipped += u64::from(sample.abs() >= 0.999);
                            dropped += capture_writer.push(sample);
                        }
                        counters.peak.store(peak.to_bits(), Ordering::Relaxed);
                        counters.counts.input_samples.add(480);
                        counters.counts.input_clipped.add(clipped);
                        counters.counts.capture_overflow.add(dropped);
                        counters.dropped.fetch_add(dropped, Ordering::Relaxed);
                    }
                }
                next += Duration::from_millis(10);
                let now = Instant::now();
                if next < now {
                    next = now;
                } // Do not invent captured audio after a scheduling stall.
                thread::sleep(next.saturating_duration_since(now));
            }
        })
        .map_err(|_| "Cannot start virtual audio devices")?;
    Ok(Devices {
        info: serde_json::json!({"input":{"name":"Test microphone loop","fallback":false},"output":{"name":"Silent speakers","fallback":false}}),
        streams: Box::new(Streams {
            playing,
            stop,
            thread: Some(thread),
        }),
        capture,
        reference,
        writers,
        control,
        health,
    })
}

fn loop_sample(kind: MicrophoneLoop, sample: u64, speech: &[f32]) -> f32 {
    if kind == MicrophoneLoop::Speech {
        return speech
            .get(sample as usize % (speech.len() + 48_000))
            .copied()
            .unwrap_or(0.0);
    }
    let time = (sample % 144_000) as f32 / 48_000.0;
    if time > 2.4 {
        return 0.0;
    }
    let frequencies = match kind {
        MicrophoneLoop::Chimes => [523.25, 659.25, 783.99, 1046.5],
        MicrophoneLoop::Pulse => [130.81, 130.81, 196.0, 164.81],
        _ => [392.0, 493.88, 587.33, 493.88],
    };
    let local = time % 0.25;
    (std::f32::consts::TAU * frequencies[(time * 4.0) as usize % 4] * local).sin()
        * (local * 100.0).min(1.0)
        * (-local * 14.0).exp()
        * 0.12
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn predetermined_loops_have_quiet_gaps_and_distinct_bounded_audio() {
        let mut previous = Vec::new();
        for kind in [
            MicrophoneLoop::Chimes,
            MicrophoneLoop::Pulse,
            MicrophoneLoop::Melody,
        ] {
            let samples: Vec<_> = (0..960).map(|n| loop_sample(kind, n, &[])).collect();
            assert_ne!(samples, previous);
            assert!(samples.iter().any(|s| s.abs() > 0.01));
            assert!(samples.iter().all(|s| s.abs() <= 0.12));
            assert_eq!(loop_sample(kind, 130_000, &[]), 0.0);
            previous = samples;
        }
        let speech = [0.1, -0.2, 0.3];
        assert_eq!(loop_sample(MicrophoneLoop::Speech, 1, &speech), -0.2);
        assert_eq!(loop_sample(MicrophoneLoop::Speech, 3, &speech), 0.0);
        assert_eq!(loop_sample(MicrophoneLoop::Speech, 48_003, &speech), 0.1);
    }
}
