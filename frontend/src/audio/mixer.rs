//! Fixed-capacity SPSC queues. The device callback never allocates, locks or waits.
use super::{clips, cues, frames, quality::Count};
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
/// Samples mixed per block; per-stream controls are read once per block (1 ms).
const MIX_BLOCK: usize = 48;

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
        // Most callback counts are zero; skip the shared-cache-line RMW.
        if n == 0 {
            return;
        }
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
    pub fn drain(&self, metrics: &impl super::quality::Sink) {
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
    pub fn drain(&self, metrics: &impl super::quality::Sink) {
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
    /// The audio worker registers itself on its first tick with these devices.
    /// Callbacks unpark it instead of letting it poll; unparking neither
    /// allocates nor locks.
    pub(super) worker: OnceLock<std::thread::Thread>,
    pub counts: CallbackCounts,
    callback_samples: AtomicUsize,
    callback_origin: Instant,
    callback_deadline_ns: AtomicU64,
    pub(super) cues: AtomicU8,
    pub(super) cue_volumes: [AtomicU8; 6],
    /// Control-worker side of this device's soundboard queue; never locked by the callback.
    pub(super) clips: std::sync::Mutex<Option<clips::Sender>>,
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
impl Controls {
    pub(super) fn wake(&self) {
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            receive_guard: OnceLock::new(),
            worker: OnceLock::new(),
            counts: Default::default(),
            callback_samples: 0.into(),
            callback_origin: Instant::now(),
            callback_deadline_ns: 0.into(),
            cues: 0.into(),
            cue_volumes: std::array::from_fn(|_| AtomicU8::new(100)),
            clips: std::sync::Mutex::new(None),
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
    clips: clips::Player,
    streams: Vec<StreamReader>,
    controls: Arc<Controls>,
    reference: Option<frames::Writer>,
    /// 48 kHz samples rendered since the worker was last woken for refills.
    since_wake: usize,
    /// Each stream's samples for the current mix block, allocated with the mixer.
    scratch: Box<[[f32; MIX_BLOCK]; MAX_STREAMS]>,
}

/// A stream mixed into the current block, with the controls it was mixed with.
#[derive(Clone, Copy, Default)]
struct Contributor {
    index: usize,
    generation: u64,
    volume: u32,
    popped: usize,
}

/// Rebuild `shared`/`room` for block samples `from..n` from cues/clips and the
/// remaining contributors' retained samples.
fn mix_from(
    from: usize,
    n: usize,
    base: &[f32; MIX_BLOCK],
    contributors: &[Contributor],
    scratch: &[[f32; MIX_BLOCK]; MAX_STREAMS],
    shared: &mut [f32; MIX_BLOCK],
    room: &mut [f32; MIX_BLOCK],
) {
    shared[from..n].copy_from_slice(&base[from..n]);
    room[from..n].fill(0.0);
    for c in contributors {
        if c.popped <= from {
            continue;
        }
        let mix = if c.index < thiscord_shared::voice::ROOM_CAPACITY {
            &mut *room
        } else {
            &mut *shared
        };
        let volume = f32::from_bits(c.volume);
        for (mix, sample) in mix[from..c.popped]
            .iter_mut()
            .zip(&scratch[c.index][from..c.popped])
        {
            *mix += if sample.is_finite() {
                sample * volume
            } else {
                0.0
            };
        }
    }
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
    let (clip_sender, clip_player) = clips::channel();
    if let Ok(mut sender) = controls.clips.lock() {
        *sender = Some(clip_sender);
    }
    (
        writers,
        Mixer {
            cues: cues::Player::new(),
            clips: clip_player,
            streams: readers,
            controls,
            reference: None,
            since_wake: 0,
            scratch: Box::new([[0.0; MIX_BLOCK]; MAX_STREAMS]),
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
        let count = output.len().div_ceil(channels);
        let mut frames = output.chunks_mut(channels);
        self.render_mono(count, now, |sample| {
            let frame = frames.next().expect("one frame per sample");
            // Voice occupies the first stereo pair; silence other channels.
            frame.fill(T::from_sample(0.0));
            for channel in frame.iter_mut().take(2) {
                *channel = T::from_sample(sample);
            }
        });
    }

    pub(super) fn take_reference(&mut self) -> Option<frames::Writer> {
        self.reference.take()
    }

    pub(super) fn reference_overflow(&self, dropped: u64) {
        self.controls.counts.reference_overflow.add(dropped);
        self.controls
            .reference_dropped
            .fetch_add(dropped, Ordering::Relaxed);
    }

    pub(super) fn privacy(&self) -> Privacy {
        Privacy {
            controls: self.controls.clone(),
            streams: self.streams.iter().map(|s| s.control.clone()).collect(),
            initialized: false,
            global: (false, true, 0),
            previous: [(0, false, 0); MAX_STREAMS],
            watched: [0; MAX_STREAMS],
            watched_len: 0,
            mask: [false; MAX_STREAMS],
        }
    }

    pub(super) fn render_mono(&mut self, count: usize, now: Instant, emit: impl FnMut(f32)) {
        self.render_masked(count, now, &[true; MAX_STREAMS], emit);
    }

    /// `mask` limits which slots may play during this callback. The device-rate
    /// path passes the streams its per-sample privacy check watches.
    pub(super) fn render_masked(
        &mut self,
        count: usize,
        now: Instant,
        mask: &[bool; MAX_STREAMS],
        mut emit: impl FnMut(f32),
    ) {
        let callback_samples = count;
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
        let deafened = self.controls.deafen.load(Ordering::Relaxed)
            || self
                .controls
                .receive_guard
                .get()
                .is_some_and(|guard| guard.server_deafened());
        for (index, volume) in self.controls.cue_volumes.iter().enumerate() {
            self.cues.volumes[index] = volume.load(Ordering::Relaxed);
        }
        self.cues
            .request(self.controls.cues.swap(0, Ordering::Relaxed), deafened);
        self.clips.begin(deafened);
        let guard = self.controls.receive_guard.get();
        // Streams inactive at callback start stay silent until the next callback;
        // most of the fixed slots are unused, and skipping them keeps the
        // mixing work proportional to actual sources.
        let mut live = [false; MAX_STREAMS];
        // Keep the legacy local-test trim, but never trim the larger cushion
        // needed by a device callback. Hard resets still discard the whole ring.
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let generation = stream.control.generation.load(Ordering::Acquire);
            let changed = generation != stream.generation;
            stream.generation = generation;
            let receive_blocked = index < thiscord_shared::voice::ROOM_CAPACITY
                && guard.is_some_and(|guard| !guard.receive_allowed());
            let active = stream.control.active.load(Ordering::Acquire);
            live[index] = active && mask[index];
            let discard = if changed || deafened || receive_blocked || !active {
                stream.queue.occupied_len()
            } else {
                stream
                    .queue
                    .occupied_len()
                    .saturating_sub(trim_limit(callback_samples))
            };
            stream.control.counts.discarded.add(discard as u64);
            stream.queue.skip(discard);
            if stream.control.cleared.load(Ordering::Relaxed) != generation {
                stream.control.cleared.store(generation, Ordering::Release);
            }
        }
        let mut underruns = 0;
        let mut reference_dropped = 0;
        let mut clipped = 0;
        let mut starvation = [0u64; MAX_STREAMS];
        let mut unknown = [0u64; MAX_STREAMS];
        let mut silence = [0u64; MAX_STREAMS];
        for (index, live) in live.iter().enumerate() {
            if !live {
                silence[index] = count as u64;
            }
        }
        // Streams are mixed in short blocks: each block reads a stream's controls
        // once and pops its samples in bulk, so the inner loop vectorises. Before
        // every output sample the contributing streams' generation, activity and
        // gain are re-read; a change rebuilds the rest of the block from the
        // remaining contributors, so no stale audio is emitted after a cutoff.
        // Deafen, master and the urgent room cutoff are likewise observed per
        // output sample; the room cutoff also discards room queues at once.
        let mut offset = 0;
        while offset < count {
            let n = (count - offset).min(MIX_BLOCK);
            let block_deafened = self.controls.deafen.load(Ordering::Acquire)
                || guard.is_some_and(|guard| guard.server_deafened());
            let block_room_blocked = guard.is_some_and(|guard| !guard.receive_allowed());
            let block_master = f32::from_bits(self.controls.master.load(Ordering::Relaxed));
            // Cues and clips are shared audio; room voices are kept apart so a
            // cutoff observed mid-block removes only room voices.
            let mut base = [0.0_f32; MIX_BLOCK];
            for sample in &mut base[..n] {
                *sample = self.cues.next() + self.clips.next();
            }
            let mut contributors = [Contributor::default(); MAX_STREAMS];
            let mut contributing = 0;
            for (index, stream) in self.streams.iter_mut().enumerate() {
                if !live[index] {
                    continue;
                }
                let receive_blocked =
                    block_room_blocked && index < thiscord_shared::voice::ROOM_CAPACITY;
                let generation = stream.control.generation.load(Ordering::Acquire);
                if receive_blocked || generation != stream.generation {
                    let discard = stream.queue.occupied_len();
                    stream.control.counts.discarded.add(discard as u64);
                    stream.queue.skip(discard);
                    stream.generation = generation;
                    stream.control.cleared.store(generation, Ordering::Release);
                }
                if block_deafened
                    || receive_blocked
                    || !stream.control.active.load(Ordering::Relaxed)
                {
                    silence[index] += n as u64;
                    continue;
                }
                let volume = stream.control.volume.load(Ordering::Relaxed);
                let popped = stream.queue.pop_slice(&mut self.scratch[index][..n]);
                if !(block_master > 0.0 && f32::from_bits(volume) > 0.0) {
                    silence[index] += n as u64;
                    continue;
                }
                let missing = (n - popped) as u64;
                if missing > 0 {
                    if stream.control.quiet.load(Ordering::Relaxed) {
                        silence[index] += missing;
                    } else if stream.control.pending_media.load(Ordering::Relaxed) {
                        starvation[index] += missing;
                        underruns += missing;
                    } else {
                        unknown[index] += missing;
                    }
                }
                if popped > 0 {
                    contributors[contributing] = Contributor {
                        index,
                        generation,
                        volume,
                        popped,
                    };
                    contributing += 1;
                }
            }
            let mut shared = [0.0_f32; MIX_BLOCK];
            let mut room = [0.0_f32; MIX_BLOCK];
            mix_from(
                0,
                n,
                &base,
                &contributors[..contributing],
                &self.scratch,
                &mut shared,
                &mut room,
            );
            let mut room_cut = block_room_blocked;
            for i in 0..n {
                // A contributor's slot can be reassigned, deactivated or muted
                // during this callback; apply it before the next sample.
                let mut changed = false;
                let mut k = 0;
                while k < contributing {
                    let c = contributors[k];
                    let stream = &mut self.streams[c.index];
                    let generation = stream.control.generation.load(Ordering::Acquire);
                    let active = stream.control.active.load(Ordering::Relaxed);
                    let volume = stream.control.volume.load(Ordering::Relaxed);
                    if generation == c.generation && active && volume == c.volume {
                        k += 1;
                        continue;
                    }
                    changed = true;
                    if generation != c.generation {
                        let discard = stream.queue.occupied_len();
                        stream.control.counts.discarded.add(discard as u64);
                        stream.queue.skip(discard);
                        stream.generation = generation;
                        stream.control.cleared.store(generation, Ordering::Release);
                    }
                    if generation == c.generation && active && f32::from_bits(volume) > 0.0 {
                        contributors[k].volume = volume;
                        k += 1;
                    } else {
                        contributing -= 1;
                        contributors[k] = contributors[contributing];
                    }
                }
                if changed {
                    mix_from(
                        i,
                        n,
                        &base,
                        &contributors[..contributing],
                        &self.scratch,
                        &mut shared,
                        &mut room,
                    );
                }
                let deafened = self.controls.deafen.load(Ordering::Acquire)
                    || guard.is_some_and(|guard| guard.server_deafened());
                // An urgent cutoff can arrive during this callback. Observe it
                // before each microphone sample and never affect shared audio.
                if !room_cut && guard.is_some_and(|guard| !guard.receive_allowed()) {
                    room_cut = true;
                    for (stream, _) in self
                        .streams
                        .iter_mut()
                        .zip(live)
                        .take(thiscord_shared::voice::ROOM_CAPACITY)
                        .filter(|(_, live)| *live)
                    {
                        let generation = stream.control.generation.load(Ordering::Acquire);
                        let discard = stream.queue.occupied_len();
                        stream.control.counts.discarded.add(discard as u64);
                        stream.queue.skip(discard);
                        stream.generation = generation;
                        stream.control.cleared.store(generation, Ordering::Release);
                    }
                }
                let master = f32::from_bits(self.controls.master.load(Ordering::Relaxed));
                let mix = shared[i] + if room_cut { 0.0 } else { room[i] };
                if !deafened && (mix * master).abs() > 1.0 {
                    clipped += 1;
                }
                let sample = if deafened {
                    0.0
                } else {
                    (mix * master).clamp(-1.0, 1.0)
                };
                emit(sample);
                if let Some(reference) = self.reference.as_mut() {
                    reference_dropped += reference.push(sample);
                }
            }
            offset += n;
        }
        self.controls.counts.output_samples.add(count as u64);
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
        // Refill right after consumption; small callbacks wake every 10 ms.
        self.since_wake += count;
        if self.since_wake >= frames::SAMPLES {
            self.since_wake = 0;
            self.controls.wake();
        }
    }
}

// Device filters hold delayed mixed audio. Flush it on access/generation, gain,
// active-state or deafen changes, including when no new 48 kHz sample is needed.
type StreamState = (u64, bool, u32);
pub(super) struct Privacy {
    controls: Arc<Controls>,
    streams: Vec<Arc<StreamControl>>,
    initialized: bool,
    global: (bool, bool, u32),
    previous: [StreamState; MAX_STREAMS],
    /// Streams active at callback start. Only these can contribute mixed audio
    /// during the callback (see `Mixer::render_mono`), so per-sample checks skip
    /// the remaining fixed slots; `begin` compares every slot once per callback.
    watched: [u8; MAX_STREAMS],
    watched_len: usize,
    mask: [bool; MAX_STREAMS],
}
impl Privacy {
    fn global(&self) -> (bool, bool, u32) {
        let guard = self.controls.receive_guard.get();
        (
            self.controls.deafen.load(Ordering::Acquire)
                || guard.is_some_and(|guard| guard.server_deafened()),
            guard.is_none_or(|g| g.receive_allowed()),
            self.controls.master.load(Ordering::Relaxed),
        )
    }
    fn stream(&self, index: usize) -> StreamState {
        let stream = &self.streams[index];
        (
            stream.generation.load(Ordering::Acquire),
            stream.active.load(Ordering::Acquire),
            stream.volume.load(Ordering::Relaxed),
        )
    }
    /// Full comparison of every slot, run once at the start of each device callback.
    pub fn begin(&mut self) -> bool {
        let global = self.global();
        let mut changed = global != self.global;
        self.global = global;
        self.watched_len = 0;
        for index in 0..self.streams.len() {
            let state = self.stream(index);
            changed |= state != self.previous[index];
            self.previous[index] = state;
            self.mask[index] = state.1;
            if state.1 {
                self.watched[self.watched_len] = index as u8;
                self.watched_len += 1;
            }
        }
        let changed = changed && self.initialized;
        self.initialized = true;
        changed
    }
    /// Streams the mixer may render during this callback: those watched here.
    pub fn mask(&self) -> &[bool; MAX_STREAMS] {
        &self.mask
    }
    /// Per-sample check of deafen/access/master and the streams that were active
    /// at callback start. Changes to other slots are found by the next `begin`.
    pub fn changed(&mut self) -> bool {
        if !self.initialized {
            return self.begin();
        }
        let global = self.global();
        let mut changed = global != self.global;
        self.global = global;
        for &index in &self.watched[..self.watched_len] {
            let index = usize::from(index);
            let state = self.stream(index);
            changed |= state != self.previous[index];
            self.previous[index] = state;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::quality::Metrics;
    #[test]
    fn server_deafen_discards_microphone_and_system_pcm_without_changing_self_state() {
        let controls = Arc::new(Controls::default());
        let guard = Arc::new(super::super::outgoing::Control::default());
        controls.receive_guard.set(guard.clone()).ok();
        let (mut writers, mut mix) = mixer(controls.clone());
        for slot in [0, thiscord_shared::voice::ROOM_CAPACITY] {
            writers[slot].control.active.store(true, Ordering::Release);
            writers[slot].write(&[0.2; 960]);
        }
        guard.server_restrictions(false, true, true);
        let mut out = [1.0f32; 480];
        mix.render(&mut out, 1);
        assert!(out.iter().all(|sample| *sample == 0.0));
        assert!(!controls.deafen.load(Ordering::Acquire));
        guard.server_restrictions(false, false, true);
        mix.render(&mut out, 1);
        assert!(
            out.iter().all(|sample| *sample == 0.0),
            "restricted PCM survived release"
        );
    }

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
    fn soundboard_clips_follow_master_reach_echo_reference_and_stop_on_deafen() {
        let controls = Arc::new(Controls::default());
        let (_, mix) = mixer(controls.clone());
        let (producer, mut reference) = frames::queue();
        let mut mix = mix.with_reference(producer);
        controls.master.store(0.5_f32.to_bits(), Ordering::Relaxed);
        let clip: clips::Clip = vec![16_384; frames::SAMPLES * 3].into();
        let play = |clip: &clips::Clip| {
            controls
                .clips
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .play(clip.clone(), 1.0)
        };
        assert!(play(&clip));
        let mut out = [0.0_f32; frames::SAMPLES];
        mix.reference_time(Instant::now());
        mix.render(&mut out, 1);
        assert_eq!(out, [0.25; frames::SAMPLES]);
        assert_eq!(reference.pop().unwrap().0.samples, out);
        // Deafening retires the rest; the clip is released on the control side.
        controls.deafen.store(true, Ordering::Relaxed);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; frames::SAMPLES]);
        controls.deafen.store(false, Ordering::Relaxed);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; frames::SAMPLES]);
        assert_eq!(Arc::strong_count(&clip), 2);
        assert!(play(&clip));
        assert_eq!(Arc::strong_count(&clip), 2);
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
    #[test]
    fn rendering_wakes_the_registered_worker_every_ten_milliseconds() {
        let controls = Arc::new(Controls::default());
        let (_writers, mut mix) = mixer(controls.clone());
        controls.worker.set(std::thread::current()).unwrap();
        // Drain any token left by an earlier unpark of this test thread.
        std::thread::park_timeout(std::time::Duration::ZERO);
        let mut out = [0.0_f32; frames::SAMPLES - 1];
        mix.render(&mut out, 1);
        let started = Instant::now();
        std::thread::park_timeout(std::time::Duration::from_millis(200));
        assert!(started.elapsed() >= std::time::Duration::from_millis(150));
        mix.render(&mut [0.0_f32; 1], 1);
        let started = Instant::now();
        std::thread::park_timeout(std::time::Duration::from_secs(5));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
    #[test]
    fn streams_inactive_at_callback_start_count_silence_without_mixing() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mut mix) = mixer(controls);
        writers[3].write(&[0.5; 64]);
        let mut out = [0.0_f32; 64];
        mix.render(&mut out, 1);
        assert_eq!(out, [0.0; 64]);
        assert_eq!(writers[3].occupied_samples(), 0);
        writers[3].control.active.store(true, Ordering::Release);
        writers[3].write(&[0.5; 64]);
        mix.render(&mut out, 1);
        assert_eq!(out, [0.5; 64]);
    }
    #[test]
    fn urgent_room_cutoff_inside_a_mix_block_silences_room_voices_at_that_sample() {
        let controls = Arc::new(Controls::default());
        let guard = Arc::new(super::super::outgoing::Control::default());
        guard.enable_receive_protection();
        assert!(guard.admit_receive(guard.generation()));
        controls.receive_guard.set(guard.clone()).ok();
        let (mut writers, mut mix) = mixer(controls.clone());
        let shared = thiscord_shared::voice::ROOM_CAPACITY;
        for (slot, level) in [(0, 0.25), (shared, 0.125)] {
            writers[slot].control.active.store(true, Ordering::Release);
            writers[slot].write(&[level; 960]);
        }
        let mut out = Vec::new();
        mix.render_mono(MIX_BLOCK * 2, Instant::now(), |sample| {
            out.push(sample);
            if out.len() == 10 {
                guard.pause_receive();
            }
        });
        // The cutoff arrives after sample 9 was emitted, inside the first block.
        assert!(out[..10].iter().all(|s| *s == 0.375), "{out:?}");
        assert!(out[10..].iter().all(|s| *s == 0.125), "{out:?}");
        assert_eq!(writers[0].occupied_samples(), 0, "room backlog discarded");
        assert!(writers[shared].occupied_samples() > 0, "shared audio kept");
    }
    #[test]
    fn privacy_checks_watched_streams_per_sample_and_all_slots_per_callback() {
        let controls = Arc::new(Controls::default());
        let (mut writers, mut mix) = mixer(controls.clone());
        writers[0].control.active.store(true, Ordering::Release);
        let mut privacy = mix.privacy();
        assert!(!privacy.begin(), "the first snapshot is a baseline");
        assert!(privacy.mask()[0] && !privacy.mask()[1]);
        assert!(!privacy.changed());
        writers[0].volume(0.5).unwrap();
        assert!(privacy.changed(), "watched gain change");
        assert!(!privacy.changed());
        controls.master.store(0.5_f32.to_bits(), Ordering::Relaxed);
        assert!(privacy.changed(), "master change");
        // A slot that was inactive at callback start cannot be mixed in this
        // callback, so the next callback's full comparison reports it.
        writers[1].control.active.store(true, Ordering::Release);
        assert!(!privacy.changed());
        writers[1].write(&[0.5; 64]);
        let mut out = Vec::new();
        mix.render_masked(64, Instant::now(), privacy.mask(), |s| out.push(s));
        assert!(out.iter().all(|s| *s == 0.0), "unwatched slot stays silent");
        assert!(privacy.begin());
        assert!(privacy.mask()[1]);
        writers[1].write(&[0.5; 64]);
        out.clear();
        mix.render_masked(64, Instant::now(), privacy.mask(), |s| out.push(s));
        assert!(out.iter().all(|s| *s == 0.25), "{out:?}");
    }
    #[test]
    fn per_stream_cutoffs_inside_a_mix_block_apply_from_the_next_sample() {
        for cutoff in 0..3 {
            let controls = Arc::new(Controls::default());
            let (mut writers, mut mix) = mixer(controls);
            for writer in writers.iter_mut().take(2) {
                writer.control.active.store(true, Ordering::Release);
                writer.write(&[0.25; 960]);
            }
            let control = writers[0].control.clone();
            let mut out = Vec::new();
            mix.render_mono(MIX_BLOCK, Instant::now(), |sample| {
                out.push(sample);
                if out.len() == 10 {
                    match cutoff {
                        0 => {
                            control.generation.fetch_add(1, Ordering::AcqRel);
                        }
                        1 => control.active.store(false, Ordering::Release),
                        _ => control.volume.store(0.0_f32.to_bits(), Ordering::Relaxed),
                    }
                }
            });
            assert!(
                out[..10].iter().all(|s| *s == 0.5),
                "cutoff {cutoff}: {out:?}"
            );
            assert!(
                out[10..].iter().all(|s| *s == 0.25),
                "cutoff {cutoff}: {out:?}"
            );
            if cutoff == 0 {
                assert_eq!(
                    writers[0].occupied_samples(),
                    0,
                    "reassigned slot discarded"
                );
                assert!(writers[0].ready());
            }
        }
    }
}
