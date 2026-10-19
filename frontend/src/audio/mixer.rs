//! Fixed-capacity SPSC queues. The device callback never allocates, locks or waits.
use super::{
    cues, frames,
    quality::{Count, Metrics},
};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use std::time::Instant;

pub const RATE: u32 = 48_000;
pub const FRAME: usize = 960;
pub const MAX_STREAMS: usize = 32;
pub const CAPACITY: usize = FRAME * 6;

/// Leave space for one maximally stretched packet. Small device callbacks keep
/// the original 15 ms refill cushion; larger ones need a complete callback plus
/// a 12 ms worker wake allowance. All queues retain their fixed allocation.
pub(super) fn refill_limit(callback_samples: usize) -> usize {
    callback_samples
        .saturating_add(576)
        .clamp(720, CAPACITY - super::playout_rate::MAX_OUTPUT)
}

fn trim_limit(callback_samples: usize) -> usize {
    (refill_limit(callback_samples) + super::playout_rate::MAX_OUTPUT).max(FRAME * 3)
}

/// Bounded callback updates without a CAS retry loop. A wrap latches saturation
/// for the worker drain, so even artificially injected u64 overflow is visible.
#[derive(Default)]
pub struct SampleCounter {
    value: AtomicU64,
    overflow: AtomicBool,
}
impl SampleCounter {
    pub fn add(&self, n: u64) {
        if self
            .value
            .fetch_add(n, Ordering::Relaxed)
            .checked_add(n)
            .is_none()
        {
            self.overflow.store(true, Ordering::Release);
        }
    }
    fn drain(&self) -> u64 {
        let value = self.value.swap(0, Ordering::Relaxed);
        if self.overflow.swap(false, Ordering::AcqRel) {
            u64::MAX
        } else {
            value
        }
    }
}

#[derive(Default)]
pub struct CallbackCounts {
    pub capture_overflow: SampleCounter,
    pub reference_overflow: SampleCounter,
    pub input_samples: SampleCounter,
    pub input_clipped: SampleCounter,
    pub output_samples: SampleCounter,
    pub output_clipped: SampleCounter,
}
impl CallbackCounts {
    pub fn drain(&self, metrics: &Metrics) {
        for (counter, event) in [
            (&self.capture_overflow, Count::CaptureOverflowSamples),
            (&self.reference_overflow, Count::ReferenceOverflowSamples),
            (&self.input_samples, Count::InputSamples),
            (&self.input_clipped, Count::InputClippedSamples),
            (&self.output_samples, Count::OutputSamples),
            (&self.output_clipped, Count::OutputClippedSamples),
        ] {
            metrics.add(event, counter.drain());
        }
    }
}
#[derive(Default)]
pub struct PlaybackCounts {
    starvation: SampleCounter,
    unknown: SampleCounter,
    silence: SampleCounter,
    discarded: SampleCounter,
}
impl PlaybackCounts {
    pub fn clear(&self) {
        for counter in [
            &self.starvation,
            &self.unknown,
            &self.silence,
            &self.discarded,
        ] {
            counter.drain();
        }
    }
    pub fn drain(&self, metrics: &Metrics) {
        for (counter, event) in [
            (&self.starvation, Count::StarvationSamples),
            (&self.unknown, Count::EmptyUnknownSamples),
            (&self.silence, Count::IntentionalSilenceSamples),
            (&self.discarded, Count::PcmDiscardedSamples),
        ] {
            metrics.add(event, counter.drain());
        }
    }
}
pub struct Controls {
    pub(super) receive_guard: OnceLock<Arc<super::outgoing::Control>>,
    pub counts: CallbackCounts,
    callback_samples: AtomicUsize,
    callback_origin: Instant,
    callback_deadline_ns: AtomicU64,
    pub(super) cues: AtomicU8,
    pub(super) cue_volumes: [AtomicU8; 6],
    pub mute: AtomicBool,
    pub deafen: AtomicBool,
    pub push_to_talk: AtomicBool,
    pub pressed: AtomicBool,
    pub threshold: AtomicU32,
    pub master: AtomicU32,
    pub peak: AtomicU32,
    pub reference_dropped: AtomicU64,
    pub transmitting: AtomicBool,
    pub dropped: AtomicU64,
    pub underruns: AtomicU64,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            receive_guard: OnceLock::new(),
            counts: Default::default(),
            callback_samples: 0.into(),
            callback_origin: Instant::now(),
            callback_deadline_ns: 0.into(),
            cues: 0.into(),
            cue_volumes: std::array::from_fn(|_| AtomicU8::new(100)),
            mute: false.into(),
            deafen: false.into(),
            push_to_talk: false.into(),
            pressed: false.into(),
            threshold: AtomicU32::new(0.015_f32.to_bits()),
            master: AtomicU32::new(1.0_f32.to_bits()),
            peak: AtomicU32::new(0),
            reference_dropped: 0.into(),
            transmitting: false.into(),
            dropped: 0.into(),
            underruns: 0.into(),
        }
    }
}
pub struct StreamControl {
    pub counts: PlaybackCounts,
    pub pending_media: AtomicBool,
    pub quiet: AtomicBool,
    pub volume: AtomicU32,
    pub active: AtomicBool,
    pub generation: AtomicU64,
    cleared: AtomicU64,
}
pub struct StreamWriter {
    pub queue: HeapProd<f32>,
    pub control: Arc<StreamControl>,
    output: Arc<Controls>,
}
struct StreamReader {
    queue: HeapCons<f32>,
    control: Arc<StreamControl>,
    generation: u64,
}
pub struct Mixer {
    cues: cues::Player,
    streams: Vec<StreamReader>,
    controls: Arc<Controls>,
    reference: Option<frames::Writer>,
}

pub fn mixer(controls: Arc<Controls>) -> (Vec<StreamWriter>, Mixer) {
    let mut writers = Vec::with_capacity(MAX_STREAMS);
    let mut readers = Vec::with_capacity(MAX_STREAMS);
    for _ in 0..MAX_STREAMS {
        let (queue, reader) = HeapRb::<f32>::new(CAPACITY).split();
        let control = Arc::new(StreamControl {
            counts: Default::default(),
            pending_media: false.into(),
            quiet: false.into(),
            volume: AtomicU32::new(1.0_f32.to_bits()),
            active: false.into(),
            generation: 0.into(),
            cleared: 0.into(),
        });
        writers.push(StreamWriter {
            queue,
            control: control.clone(),
            output: controls.clone(),
        });
        readers.push(StreamReader {
            queue: reader,
            control,
            generation: 0,
        });
    }
    (
        writers,
        Mixer {
            cues: cues::Player::new(),
            streams: readers,
            controls,
            reference: None,
        },
    )
}
impl StreamWriter {
    pub(super) fn callback_samples(&self) -> usize {
        self.output.callback_samples.load(Ordering::Relaxed)
    }
    pub(super) fn callback_due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.output.callback_origin)
            .as_nanos()
            + 12_000_000
            >= u128::from(self.output.callback_deadline_ns.load(Ordering::Relaxed))
    }
    pub fn occupied_samples(&self) -> usize {
        self.queue.occupied_len()
    }
    pub fn discard(&self) {
        self.control.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub fn ready(&self) -> bool {
        self.control.cleared.load(Ordering::Acquire)
            == self.control.generation.load(Ordering::Acquire)
    }
    pub fn write(&mut self, samples: &[f32]) -> usize {
        self.queue.push_slice(samples)
    }
    pub fn volume(&self, gain: f32) -> Result<(), &'static str> {
        if !gain.is_finite() || !(0.0..=2.0).contains(&gain) {
            return Err("Volume must be between 0 and 200%");
        }
        self.control.volume.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}
impl Mixer {
    pub fn with_reference(mut self, reference: frames::Writer) -> Self {
        self.reference = Some(reference);
        self
    }
    pub fn reference_time(&mut self, at: Instant) {
        if let Some(reference) = &mut self.reference {
            reference.begin(at);
        }
    }
    pub fn render<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        output: &mut [T],
        channels: usize,
    ) {
        self.render_at(output, channels, Instant::now());
    }
    pub(super) fn render_at<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        output: &mut [T],
        channels: usize,
        now: Instant,
    ) {
        let callback_samples = output.len().div_ceil(channels);
        let deadline = now.saturating_duration_since(self.controls.callback_origin)
            + frames::sample_duration(callback_samples.min(CAPACITY));
        self.controls.callback_deadline_ns.store(
            deadline.as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        // One bounded atomic update, shared by every source. Retain the largest
        // callback for this device lifetime so variable callback sizes cannot
        // repeatedly shrink the worker's cushion before a larger callback.
        let callback_samples = callback_samples
            .max(self.controls.callback_samples.load(Ordering::Relaxed))
            .min(CAPACITY);
        self.controls
            .callback_samples
            .store(callback_samples, Ordering::Relaxed);
        let deafened = self.controls.deafen.load(Ordering::Relaxed);
        for (index, volume) in self.controls.cue_volumes.iter().enumerate() {
            self.cues.volumes[index] = volume.load(Ordering::Relaxed);
        }
        self.cues
            .request(self.controls.cues.swap(0, Ordering::Relaxed), deafened);
        let master = f32::from_bits(self.controls.master.load(Ordering::Relaxed));
        // Keep the legacy local-test trim, but never trim the larger cushion
        // needed by a device callback. Hard resets still discard the whole ring.
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let generation = stream.control.generation.load(Ordering::Acquire);
            let changed = generation != stream.generation;
            stream.generation = generation;
            let receive_blocked = index < thiscord_shared::voice::ROOM_CAPACITY
                && self
                    .controls
                    .receive_guard
                    .get()
                    .is_some_and(|guard| !guard.receive_allowed());
            let discard = if changed
                || deafened
                || receive_blocked
                || !stream.control.active.load(Ordering::Acquire)
            {
                stream.queue.occupied_len()
            } else {
                stream
                    .queue
                    .occupied_len()
                    .saturating_sub(trim_limit(callback_samples))
            };
            stream.control.counts.discarded.add(discard as u64);
            for _ in 0..discard {
                let _ = stream.queue.try_pop();
            }
            stream.control.cleared.store(generation, Ordering::Release);
        }
        let mut underruns = 0;
        let mut reference_dropped = 0;
        let mut clipped = 0;
        let mut starvation = [0u64; MAX_STREAMS];
        let mut unknown = [0u64; MAX_STREAMS];
        let mut silence = [0u64; MAX_STREAMS];
        for frame in output.chunks_mut(channels) {
            let mut mix = self.cues.next();
            for (index, stream) in self.streams.iter_mut().enumerate() {
                // An urgent cutoff can arrive during this callback. Observe it
                // before each microphone sample and never affect shared audio.
                let receive_blocked = index < thiscord_shared::voice::ROOM_CAPACITY
                    && self
                        .controls
                        .receive_guard
                        .get()
                        .is_some_and(|guard| !guard.receive_allowed());
                let generation = stream.control.generation.load(Ordering::Acquire);
                if receive_blocked || generation != stream.generation {
                    let discard = stream.queue.occupied_len();
                    stream.control.counts.discarded.add(discard as u64);
                    for _ in 0..discard {
                        let _ = stream.queue.try_pop();
                    }
                    stream.generation = generation;
                    stream.control.cleared.store(generation, Ordering::Release);
                }
                if deafened || receive_blocked || !stream.control.active.load(Ordering::Relaxed) {
                    silence[index] += 1;
                    continue;
                }
                let audible = master > 0.0
                    && f32::from_bits(stream.control.volume.load(Ordering::Relaxed)) > 0.0;
                if !audible {
                    silence[index] += 1;
                }
                match stream.queue.try_pop() {
                    Some(sample) if sample.is_finite() => {
                        mix +=
                            sample * f32::from_bits(stream.control.volume.load(Ordering::Relaxed))
                    }
                    Some(_) => {}
                    None if audible && stream.control.quiet.load(Ordering::Relaxed) => {
                        silence[index] += 1;
                    }
                    None if audible && stream.control.pending_media.load(Ordering::Relaxed) => {
                        starvation[index] += 1;
                        underruns += 1;
                    }
                    None if audible => unknown[index] += 1,
                    None => {}
                }
            }
            if !deafened && (mix * master).abs() > 1.0 {
                clipped += 1;
            }
            let sample = if deafened {
                0.0
            } else {
                (mix * master).clamp(-1.0, 1.0)
            };
            // Device layouts may expose 5.1/7.1 or interface channels. Place
            // voice in the first stereo pair (mono if one channel); leave the
            // remaining channels, including any LFE channel, silent.
            frame.fill(T::from_sample(0.0));
            for channel in frame.iter_mut().take(2) {
                *channel = T::from_sample(sample);
            }
            if let Some(reference) = &mut self.reference {
                reference_dropped += reference.push(sample);
            }
        }
        self.controls
            .counts
            .output_samples
            .add(output.len().div_ceil(channels) as u64);
        self.controls.counts.output_clipped.add(clipped);
        for (i, stream) in self.streams.iter().enumerate() {
            stream.control.counts.starvation.add(starvation[i]);
            stream.control.counts.unknown.add(unknown[i]);
            stream.control.counts.silence.add(silence[i]);
        }
        self.controls
            .underruns
            .fetch_add(underruns, Ordering::Relaxed);
        self.controls
            .counts
            .reference_overflow
            .add(reference_dropped);
        self.controls
            .reference_dropped
            .fetch_add(reference_dropped, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_counter_wrap_latches_saturation_without_retrying() {
        let counter = SampleCounter::default();
        counter.add(u64::MAX);
        counter.add(2);
        assert_eq!(counter.drain(), u64::MAX);
        assert_eq!(counter.drain(), 0);
        counter.add(480);
        assert_eq!(counter.drain(), 480);
    }
    #[test]
    fn intentional_silence_and_unknown_inactivity_are_not_playback_starvation() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mut mix) = mixer(controls.clone());
        let metrics = Metrics::default();
        let mut out = [0f32; 480];
        mix.render(&mut out, 1); // inactive
        writers[0].control.active.store(true, Ordering::Release);
        mix.render(&mut out, 1); // empty, no evidence of pending media (DTX/inactivity)
        writers[0].write(&[0.0; 480]);
        mix.render(&mut out, 1); // normal decoded silence
        controls.mute.store(true, Ordering::Relaxed);
        mix.render(&mut out, 1); // local mute does not imply missing received speech
        writers[0]
            .control
            .pending_media
            .store(true, Ordering::Relaxed);
        controls.deafen.store(true, Ordering::Relaxed);
        mix.render(&mut out, 1);
        controls.deafen.store(false, Ordering::Relaxed);
        writers[0].volume(0.0).unwrap();
        mix.render(&mut out, 1);
        writers[0].volume(1.0).unwrap();
        controls.master.store(0f32.to_bits(), Ordering::Relaxed);
        mix.render(&mut out, 1);
        writers[0].control.counts.drain(&metrics);
        let t = metrics.snapshot().total;
        assert_eq!(t.starvation_samples, 0);
        assert_eq!(t.empty_unknown_samples, 960);
        assert_eq!(t.intentional_silence_samples, 1920);
        assert_eq!(controls.underruns.load(Ordering::Relaxed), 0);
        controls.master.store(1f32.to_bits(), Ordering::Relaxed);
        mix.render(&mut out, 1); // known buffered media, worker has not decoded it
        writers[0].control.counts.drain(&metrics);
        assert_eq!(metrics.snapshot().total.starvation_samples, 480);
        assert_eq!(controls.underruns.load(Ordering::Relaxed), 480);
        writers[0].control.quiet.store(true, Ordering::Relaxed);
        mix.render(&mut out, 1); // Explicit DTX silence overrides stale pending-media evidence.
        writers[0].control.counts.drain(&metrics);
        assert_eq!(metrics.snapshot().total.starvation_samples, 480);
        assert_eq!(metrics.snapshot().total.intentional_silence_samples, 2400);
    }
    #[test]
    fn clipping_reference_overflow_and_stream_counts_are_exact_and_distinct() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mix) = mixer(controls.clone());
        let (reference, _reader) = frames::queue();
        let mut mix = mix.with_reference(reference);
        let a = Metrics::default();
        let b = Metrics::default();
        let device = Metrics::default();
        for w in writers.iter_mut().take(2) {
            w.control.active.store(true, Ordering::Release);
        }
        writers[0].volume(2.0).unwrap();
        writers[0].write(&[0.8; 480]);
        writers[1]
            .control
            .pending_media
            .store(true, Ordering::Relaxed);
        let mut out = [0f32; 960];
        mix.render(&mut out, 2);
        for _ in 0..12 {
            mix.render(&mut out, 2);
        }
        controls.counts.drain(&device);
        writers[0].control.counts.drain(&a);
        writers[1].control.counts.drain(&b);
        assert_eq!(device.snapshot().total.output_clipped_samples, 480);
        assert_eq!(device.snapshot().total.output_samples, 13 * 480);
        assert_eq!(device.snapshot().total.reference_overflow_samples, 480);
        assert_eq!(device.snapshot().total.capture_overflow_samples, 0);
        assert_eq!(a.snapshot().total.starvation_samples, 0);
        assert_eq!(b.snapshot().total.starvation_samples, 13 * 480);
        controls.counts.drain(&device); // no double accounting
        assert_eq!(device.snapshot().total.output_samples, 13 * 480);
    }
    #[test]
    fn membership_cues_follow_master_and_reach_echo_reference() {
        let controls = Arc::new(Controls::default());
        let (_, mix) = mixer(controls.clone());
        let (producer, mut reference) = frames::queue();
        let mut mix = mix.with_reference(producer);
        controls.master.store(0.5_f32.to_bits(), Ordering::Relaxed);
        controls.cues.store(cues::JOIN, Ordering::Relaxed);
        let mut out = [0.0_f32; frames::SAMPLES];
        mix.reference_time(Instant::now());
        mix.render(&mut out, 1);
        assert!(out.iter().any(|s| s.abs() > 0.001));
        assert!(out.iter().all(|s| s.abs() <= 0.06));
        assert_eq!(reference.pop().unwrap().0.samples, out);
        controls.deafen.store(true, Ordering::Relaxed);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; frames::SAMPLES]);
        controls.deafen.store(false, Ordering::Relaxed);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; frames::SAMPLES]);
    }

    #[test]
    fn echo_reference_matches_post_volume_clipped_playback_and_deafen() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mix) = mixer(controls.clone());
        let (producer, mut reference) = frames::queue();
        let mut mix = mix.with_reference(producer);
        let at = Instant::now();
        writers[0].control.active.store(true, Ordering::Release);
        writers[0].volume(2.0).unwrap();
        controls.master.store(1.5_f32.to_bits(), Ordering::Relaxed);
        writers[0].write(&[0.4; frames::SAMPLES]);
        let mut out = [0.0_f32; frames::SAMPLES * 2];
        mix.reference_time(at);
        mix.render(&mut out, 2);
        let (frame, gap) = reference.pop().unwrap();
        assert!(!gap);
        assert_eq!(frame.at, at);
        assert_eq!(frame.samples, [1.0; frames::SAMPLES]);
        for (sample, pair) in frame.samples.iter().zip(out.as_chunks::<2>().0) {
            assert_eq!(pair, &[*sample, *sample]);
        }
        controls.deafen.store(true, Ordering::Relaxed);
        writers[0].write(&[0.4; frames::SAMPLES]);
        mix.reference_time(at + std::time::Duration::from_millis(10));
        mix.render(&mut out, 2);
        assert_eq!(reference.pop().unwrap().0.samples, [0.0; frames::SAMPLES]);
    }
    #[test]
    fn surround_output_uses_first_pair_and_silences_remaining_channels() {
        let (mut writers, mut mix) = mixer(Arc::new(Controls::default()));
        writers[0].control.active.store(true, Ordering::Release);
        writers[0].write(&[0.5, -0.5]);
        let mut out = [0_u16; 12];
        mix.render(&mut out, 6);
        assert_eq!(
            out,
            [
                49152, 49152, 32768, 32768, 32768, 32768, 16384, 16384, 32768, 32768, 32768, 32768
            ]
        );
    }
    #[test]
    fn signed_24_and_32_bit_output_preserve_level_and_polarity() {
        let (mut writers, mut mix) = mixer(Arc::new(Controls::default()));
        writers[0].control.active.store(true, Ordering::Release);
        writers[0].write(&[0.5, -0.5, 0.5, -0.5]);
        let mut out24 = [cpal::I24::new(0).unwrap(); 2];
        mix.render(&mut out24, 1);
        assert_eq!(out24.map(|v| v.inner()), [4194304, -4194304]);
        let mut out32 = [0_i32; 2];
        mix.render(&mut out32, 1);
        assert_eq!(out32, [1073741824, -1073741824]);
    }
    #[test]
    fn streams_have_independent_gain_and_deafen_discards_backlog() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mut mix) = mixer(controls.clone());
        for w in writers.iter_mut().take(2) {
            w.control.active.store(true, Ordering::Release);
            w.write(&[0.25; 4]);
        }
        writers[0].volume(0.0).unwrap();
        writers[1].volume(2.0).unwrap();
        let mut out = [0.0_f32; 4];
        mix.render(&mut out, 2);
        assert_eq!(out, [0.5; 4]);
        controls.deafen.store(true, Ordering::Relaxed);
        mix.render(&mut out, 2);
        assert_eq!(out, [0.0; 4]);
        controls.deafen.store(false, Ordering::Relaxed);
        mix.render(&mut out, 2);
        assert_eq!(out, [0.0; 4]);
        assert!(writers[0].volume(f32::NAN).is_err());
    }
    #[test]
    fn reusing_a_stream_discards_the_previous_speaker() {
        let (mut writers, mut mix) = mixer(Arc::new(Controls::default()));
        writers[0].control.active.store(true, Ordering::Release);
        writers[0].write(&[0.25; 4]);
        writers[0].control.generation.fetch_add(1, Ordering::AcqRel);
        let mut out = [0.0_f32; 2];
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; 2]);
        writers[0].write(&[0.5; 2]);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.5; 2]);
    }
    #[test]
    fn queue_is_bounded_and_mixing_clips() {
        let (mut writers, mut mix) = mixer(Arc::new(Controls::default()));
        writers[0].control.active.store(true, Ordering::Release);
        assert_eq!(writers[0].write(&vec![0.8; CAPACITY + 100]), CAPACITY);
        writers[0].volume(2.0).unwrap();
        let mut out = [0.0_f32; 1];
        mix.render(&mut out, 1);
        assert_eq!(out, [1.0]);
    }
}
