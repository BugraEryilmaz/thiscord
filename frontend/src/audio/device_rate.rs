//! Fixed-ratio device conversion. All allocation/filter design happens before
//! stream startup. The integer phase clock survives arbitrary callback sizes.
use std::time::{Duration, Instant};

pub(super) const MIN_RATE: u32 = 8_000;
pub(super) const MAX_RATE: u32 = 384_000;
const PHASES: usize = 256;
const BASE_TAPS: usize = 64;

pub(super) struct Converter {
    source: u32,
    target: u32,
    phase: u64,
    taps: usize,
    coefficients: Vec<f32>,
    history: Vec<f32>,
    cursor: usize,
    expected: Option<Instant>,
}

impl Converter {
    pub fn new(source: u32, target: u32) -> Self {
        assert!((MIN_RATE..=MAX_RATE).contains(&source));
        assert!((MIN_RATE..=MAX_RATE).contains(&target));
        let taps = BASE_TAPS * source.div_ceil(target) as usize;
        let cutoff = 0.94 * (f64::from(target) / f64::from(source)).min(1.0);
        let mut coefficients = vec![0.0; (PHASES + 1) * taps];
        for (phase, row) in coefficients.chunks_exact_mut(taps).enumerate() {
            for (i, tap) in row.iter_mut().enumerate() {
                let distance = i as f64 - (taps / 2) as f64 + phase as f64 / PHASES as f64;
                let sinc = if distance.abs() < 1e-10 {
                    cutoff
                } else {
                    let angle = std::f64::consts::PI * distance;
                    (angle * cutoff).sin() / angle
                };
                let angle = std::f64::consts::PI * distance / (taps / 2) as f64;
                let window = 0.42 + 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos();
                *tap = (sinc * window) as f32;
            }
            let sum: f32 = row.iter().sum();
            for tap in row {
                *tap /= sum;
            }
        }
        Self {
            source,
            target,
            phase: u64::from(target),
            taps,
            coefficients,
            history: vec![0.0; taps],
            cursor: 0,
            expected: None,
        }
    }

    pub fn latency(&self) -> Duration {
        Duration::from_secs_f64((self.taps / 2) as f64 / f64::from(self.source))
    }

    /// Time of the next output relative to the next input, including group delay.
    pub fn capture_time(&mut self, mut at: Instant, samples: usize) -> Instant {
        if let Some(expected) = self.expected {
            if at.saturating_duration_since(expected) > Duration::from_millis(2) {
                self.clear();
                self.phase = u64::from(self.target);
            } else if expected.saturating_duration_since(at) > Duration::from_millis(2) {
                // Batched/repeated device timestamps still represent new PCM.
                // Match Writer's monotonic handling instead of inventing a gap
                // (and resetting the filter) when timestamps catch up again.
                at = expected;
            }
        }
        self.expected = Some(at + Duration::from_secs_f64(samples as f64 / f64::from(self.source)));
        let offset = Duration::from_secs_f64(
            self.phase.saturating_sub(u64::from(self.target)) as f64
                / f64::from(self.target)
                / f64::from(self.source),
        );
        at + offset - self.latency()
    }

    /// Clear delayed audio without changing the rational clock (privacy cutoff).
    pub fn clear(&mut self) {
        self.history.fill(0.0);
    }

    /// Input samples needed to produce exactly `outputs` more output samples.
    pub fn input_needed(&self, outputs: usize) -> usize {
        if outputs == 0 {
            0
        } else {
            ((self.phase + (outputs as u64 - 1) * u64::from(self.source)) / u64::from(self.target))
                as usize
        }
    }

    pub fn push(&mut self, sample: f32) {
        debug_assert!(self.phase >= u64::from(self.target));
        self.phase -= u64::from(self.target);
        self.cursor = (self.cursor + 1) % self.taps;
        self.history[self.cursor] = sample;
    }

    pub fn pop(&mut self) -> Option<f32> {
        if self.phase >= u64::from(self.target) {
            return None;
        }
        let fraction = self.phase as f64 * PHASES as f64 / f64::from(self.target);
        let phase = fraction as usize;
        let weight = fraction.fract() as f32;
        let left = &self.coefficients[phase * self.taps..][..self.taps];
        let right = &self.coefficients[(phase + 1) * self.taps..][..self.taps];
        let mut sample = 0.0;
        for (i, (&left, &right)) in left.iter().zip(right).enumerate() {
            sample += (left + weight * (right - left))
                * self.history[(self.cursor + self.taps - i) % self.taps];
        }
        self.phase += u64::from(self.source);
        Some(sample)
    }
}

/// Converts only after mixing. Sender-clock correction remains on the worker.
/// The echo reference is derived from the actual, clamped device signal and
/// converted back to 48 kHz, with its filter delay removed from its timestamps.
pub(super) struct Playback {
    converter: Converter,
    reference_converter: Converter,
    reference: Option<super::frames::Writer>,
    privacy: super::mixer::Privacy,
}

impl Playback {
    pub fn new(rate: u32, mixer: &mut super::mixer::Mixer) -> Self {
        Self {
            converter: Converter::new(super::mixer::RATE, rate),
            reference_converter: Converter::new(rate, super::mixer::RATE),
            reference: mixer.take_reference(),
            privacy: mixer.privacy(),
        }
    }

    pub fn render<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        mixer: &mut super::mixer::Mixer,
        output: &mut [T],
        channels: usize,
        playback: Instant,
        now: Instant,
    ) {
        let count = output.len() / channels;
        let Self {
            converter,
            reference_converter,
            reference,
            privacy,
        } = self;
        if let Some(reference) = reference {
            reference.begin(reference_converter.capture_time(playback, count));
        }
        let mut frames = output.chunks_exact_mut(channels);
        let mut remaining = count;
        let mut dropped = 0;
        let mut drain = |converter: &mut Converter| {
            while remaining > 0 {
                if privacy.changed() {
                    converter.clear();
                    reference_converter.clear();
                }
                let Some(sample) = converter.pop() else { break };
                let sample = sample.clamp(-1.0, 1.0);
                let frame = frames.next().expect("one frame per converted sample");
                frame.fill(T::from_sample(0.0));
                for channel in frame.iter_mut().take(2) {
                    *channel = T::from_sample(sample);
                }
                if let Some(reference) = reference.as_mut() {
                    reference_converter.push(sample);
                    while let Some(sample) = reference_converter.pop() {
                        dropped += reference.push(sample);
                    }
                }
                remaining -= 1;
            }
            remaining
        };
        let remaining = drain(converter);
        let needed = converter.input_needed(remaining);
        mixer.render_mono(needed, now, |sample| {
            converter.push(sample);
            drain(converter);
        });
        mixer.reference_overflow(dropped);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        frames,
        mixer::{self, RATE},
        outgoing,
    };
    use super::*;
    use std::sync::{Arc, atomic::Ordering};

    const RATES: &[u32] = &[
        8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 47_999, 88_200, 96_000, 176_400,
        192_000, 352_800, 384_000,
    ];

    #[test]
    fn rational_clock_preserves_sample_counts_for_every_rate() {
        for &device in RATES {
            for (source, target) in [(device, RATE), (RATE, device)] {
                let mut converter = Converter::new(source, target);
                let mut count = 0;
                for _ in 0..source {
                    converter.push(0.1);
                    while converter.pop().is_some() {
                        count += 1;
                    }
                }
                assert_eq!(count, target, "{source} -> {target}");
            }
        }
    }

    #[test]
    fn fractional_capture_callbacks_keep_media_positions_and_timestamps() {
        for source in [11_025, 24_000, 44_100, 96_000, 192_000] {
            let mut converter = Converter::new(source, RATE);
            let (mut writer, mut reader) = frames::queue();
            let at = Instant::now();
            let origin = at - converter.latency();
            let mut supplied = 0;
            let mut consumed = 0;
            let mut block = 0;
            while supplied < source {
                let count = [1, 37, 113, 203, 17][block % 5].min((source - supplied) as usize);
                let capture = at + Duration::from_secs_f64(f64::from(supplied) / f64::from(source));
                writer.begin_capture(converter.capture_time(capture, count));
                for _ in 0..count {
                    converter.push(0.25);
                    while let Some(sample) = converter.pop() {
                        writer.push(sample);
                    }
                }
                supplied += count as u32;
                while let Some((frame, gap)) = reader.pop() {
                    assert!(!gap, "{source} at {consumed}");
                    assert_eq!(frame.position, consumed);
                    let expected = origin + frames::sample_duration(consumed as usize);
                    let error = frame.at.saturating_duration_since(expected)
                        + expected.saturating_duration_since(frame.at);
                    assert!(error <= Duration::from_nanos(2), "{source}: {error:?}");
                    consumed += 480;
                }
                block += 1;
            }
            assert_eq!(consumed, u64::from(RATE));
        }
    }

    #[test]
    fn conversion_preserves_passband_and_suppresses_aliases() {
        for (source, target) in [
            (8_000, RATE),
            (24_000, RATE),
            (44_100, RATE),
            (192_000, RATE),
            (RATE, 8_000),
            (RATE, 44_100),
            (RATE, 192_000),
        ] {
            let mut converter = Converter::new(source, target);
            let hz = f64::from(source.min(target)) * 0.18;
            let mut output = 0;
            let mut energy = 0.0;
            let mut error = 0.0;
            for i in 0..source / 5 {
                let input = (std::f64::consts::TAU * hz * f64::from(i) / f64::from(source)).sin();
                converter.push(input as f32);
                while let Some(actual) = converter.pop() {
                    let time =
                        f64::from(output) / f64::from(target) - converter.latency().as_secs_f64();
                    if i > converter.taps as u32 {
                        let expected = (std::f64::consts::TAU * hz * time).sin();
                        energy += expected * expected;
                        error += (f64::from(actual) - expected).powi(2);
                    }
                    output += 1;
                }
            }
            let snr = 10.0 * (energy / error).log10();
            assert!(snr > 55.0, "{source} -> {target}: SNR {snr}");
        }
        for (source, target) in [
            (RATE, 8_000),
            (RATE, 24_000),
            (96_000, RATE),
            (192_000, RATE),
        ] {
            let mut converter = Converter::new(source, target);
            let mut peak = 0.0_f32;
            for i in 0..source / 5 {
                let hz = f64::from(target) * 0.7;
                converter.push(
                    (std::f64::consts::TAU * hz * f64::from(i) / f64::from(source)).sin() as f32,
                );
                while let Some(sample) = converter.pop() {
                    if i > converter.taps as u32 {
                        peak = peak.max(sample.abs());
                    }
                }
            }
            assert!(peak < 0.001, "{source} -> {target}: alias peak {peak}");
        }
    }

    #[test]
    fn capture_interruption_clears_history() {
        let mut converter = Converter::new(44_100, RATE);
        let at = Instant::now();
        converter.capture_time(at, 441);
        for _ in 0..441 {
            converter.push(0.5);
            while converter.pop().is_some() {}
        }
        converter.capture_time(at + Duration::from_secs(1), 441);
        for _ in 0..441 {
            converter.push(0.0);
            while let Some(sample) = converter.pop() {
                assert_eq!(sample, 0.0);
            }
        }
    }

    #[test]
    fn repeated_device_timestamps_do_not_invent_capture_holes() {
        let mut converter = Converter::new(44_100, RATE);
        let (mut writer, mut reader) = frames::queue();
        let at = Instant::now();
        let origin = at - converter.latency();
        for (index, millis) in [0, 0, 20, 20, 40, 50, 60, 60].into_iter().enumerate() {
            writer.begin_capture(converter.capture_time(at + Duration::from_millis(millis), 441));
            for _ in 0..441 {
                converter.push(0.25);
                while let Some(sample) = converter.pop() {
                    writer.push(sample);
                }
            }
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap);
            assert_eq!(frame.position, (index * 480) as u64);
            assert_eq!(frame.at, origin + Duration::from_millis(index as u64 * 10));
            if index > 0 {
                assert!(frame.samples.iter().all(|s| (*s - 0.25).abs() < 1e-6));
            }
        }
        writer.begin_capture(converter.capture_time(at + Duration::from_millis(100), 441));
        for _ in 0..441 {
            converter.push(0.0);
            while let Some(sample) = converter.pop() {
                writer.push(sample);
            }
        }
        let (frame, gap) = reader.pop().unwrap();
        assert!(gap);
        assert_eq!(frame.position, 4800);
        assert_eq!(frame.at, origin + Duration::from_millis(100));
        assert!(frame.samples.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn pull_conversion_matches_continuous_push_across_single_sample_callbacks() {
        for &target in RATES {
            let mut pull = Converter::new(RATE, target);
            let mut push = Converter::new(RATE, target);
            let mut expected = Vec::new();
            let mut actual = Vec::new();
            let mut push_supplied = 0;
            let mut pull_supplied = 0;
            for count in [1, 1, 1, 1, 37, 113, 127, 1, 1, 203] {
                let needed = pull.input_needed(count);
                for _ in 0..needed {
                    let sample = (push_supplied as f32 * 0.123).sin();
                    push.push(sample);
                    while let Some(sample) = push.pop() {
                        expected.push(sample);
                    }
                    push_supplied += 1;
                }
                for _ in 0..count {
                    loop {
                        if let Some(sample) = pull.pop() {
                            actual.push(sample);
                            break;
                        }
                        let sample = (pull_supplied as f32 * 0.123).sin();
                        pull.push(sample);
                        pull_supplied += 1;
                    }
                }
                assert_eq!(pull_supplied, push_supplied);
            }
            assert_eq!(
                actual.as_slice(),
                &expected[..actual.len()],
                "rate {target}"
            );
        }
    }

    #[test]
    fn playback_handles_irregular_callbacks_and_writes_actual_echo_reference() {
        for rate in [8_000, 24_000, 44_100, 96_000, 192_000] {
            let controls = Arc::new(mixer::Controls::default());
            let (mut writers, mix) = mixer::mixer(controls);
            let (reference, mut reader) = frames::queue();
            let mut mix = mix.with_reference(reference);
            let mut playback = Playback::new(rate, &mut mix);
            let writer = &mut writers[0];
            writer.control.active.store(true, Ordering::Release);
            writer.write(&[0.25; 4800]);
            let at = Instant::now();
            let mut samples = 0;
            let mut read = 0;
            let mut check = Converter::new(rate, RATE);
            let mut expected = Vec::new();
            for count in [1, 37, 203, 113, 127, 521] {
                let mut data = vec![0.0_f32; count * 6];
                let time = at + Duration::from_secs_f64(samples as f64 / f64::from(rate));
                playback.render(&mut mix, &mut data, 6, time, time);
                for frame in data.as_chunks::<6>().0 {
                    assert_eq!(frame[0], frame[1]);
                    assert_eq!(&frame[2..], &[0.0; 4]);
                    check.push(frame[0]);
                    while let Some(sample) = check.pop() {
                        expected.push(sample);
                    }
                }
                samples += count;
                while let Some((frame, gap)) = reader.pop() {
                    assert!(!gap);
                    assert_eq!(frame.position, read as u64);
                    assert_eq!(frame.samples.as_slice(), &expected[read..read + 480]);
                    let expected_time = at - check.latency() + frames::sample_duration(read);
                    assert!(
                        frame.at.saturating_duration_since(expected_time)
                            + expected_time.saturating_duration_since(frame.at)
                            <= Duration::from_nanos(2)
                    );
                    read += 480;
                }
            }
            assert!(expected.iter().any(|s| s.abs() > 0.1));
        }
    }

    #[test]
    fn server_deafen_clears_system_audio_resampler_while_microphone_is_already_paused() {
        let controls = Arc::new(mixer::Controls::default());
        let guard = Arc::new(outgoing::Control::default());
        guard.enable_receive_protection(); // Microphone receive is already paused.
        controls.receive_guard.set(guard.clone()).ok();
        let (mut writers, mut mix) = mixer::mixer(controls.clone());
        let writer = &mut writers[thiscord_shared::voice::ROOM_CAPACITY];
        writer.control.active.store(true, Ordering::Release);
        writer.write(&[0.5; 4800]);
        let mut playback = Playback::new(192_000, &mut mix);
        let at = Instant::now();
        let mut warmup = [0.0_f32; 1001];
        playback.render(&mut mix, &mut warmup, 1, at, at);
        assert!(warmup[1000] > 0.1);
        guard.server_restrictions(false, true, true);
        let mut after = [1.0_f32; 257];
        playback.render(&mut mix, &mut after, 1, at + Duration::from_millis(6), at);
        assert!(after.iter().all(|s| *s == 0.0));
        guard.server_restrictions(false, false, true);
        playback.render(&mut mix, &mut after, 1, at + Duration::from_millis(8), at);
        assert!(after.iter().all(|s| *s == 0.0));
        assert!(!controls.deafen.load(Ordering::Acquire));
    }

    #[test]
    fn playback_cutoffs_remove_delayed_audio_even_between_source_samples() {
        for cutoff in 0..5 {
            let controls = Arc::new(mixer::Controls::default());
            let guard = Arc::new(outgoing::Control::default());
            guard.enable_receive_protection();
            assert!(guard.admit_receive(guard.generation()));
            controls.receive_guard.set(guard.clone()).ok();
            let (mut writers, mut mix) = mixer::mixer(controls.clone());
            let writer = &mut writers[0];
            writer.control.active.store(true, Ordering::Release);
            writer.write(&[0.5; 4800]);
            let mut playback = Playback::new(192_000, &mut mix);
            let at = Instant::now();
            let mut warmup = [0.0_f32; 1001];
            playback.render(&mut mix, &mut warmup, 1, at, at);
            assert!(warmup[1000] > 0.1);
            match cutoff {
                0 => controls.deafen.store(true, Ordering::Release),
                1 => {
                    writer.discard();
                }
                2 => guard.pause_receive(),
                3 => writer.control.active.store(false, Ordering::Release),
                _ => writer.volume(0.0).unwrap(),
            }
            let mut after = [1.0_f32; 257];
            playback.render(&mut mix, &mut after, 1, at + Duration::from_millis(6), at);
            assert!(after.iter().all(|s| *s == 0.0), "cutoff {cutoff}");
        }
    }
}
