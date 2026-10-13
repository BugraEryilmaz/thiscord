//! Timestamped 10 ms blocks, assembled without allocating on device callbacks.
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::time::{Duration, Instant};

use super::mixer::{CAPACITY, RATE};

pub const SAMPLES: usize = 480;
pub const BLOCKS: usize = CAPACITY / SAMPLES;

#[derive(Clone, Copy)]
pub struct Frame {
    pub samples: [f32; SAMPLES],
    pub at: Instant,
    pub position: u64,
}

pub struct Writer {
    queue: HeapProd<Frame>,
    frame: Frame,
    used: usize,
    callback_at: Instant,
    offset: usize,
    position: u64,
    capture_started: bool,
}

pub struct Reader {
    queue: HeapCons<Frame>,
    next: u64,
    // Allocate with the ring, not on callback or gap handling. Keep Devices movable.
    pending: Box<Option<(Frame, bool)>>,
}

pub fn queue() -> (Writer, Reader) {
    let (producer, consumer) = HeapRb::new(BLOCKS).split();
    let now = Instant::now();
    (
        Writer {
            queue: producer,
            frame: Frame {
                samples: [0.0; SAMPLES],
                at: now,
                position: 0,
            },
            used: 0,
            callback_at: now,
            offset: 0,
            position: 0,
            capture_started: false,
        },
        Reader {
            queue: consumer,
            next: 0,
            pending: Box::new(None),
        },
    )
}

impl Writer {
    /// Capture-only discontinuity handling; the AEC playback reference keeps its
    /// original timing/assembly. Follow <=2 ms mapping jitter/clock drift, but
    /// never reanchor on a larger regression and turn its recovery into a hole.
    pub fn begin_capture(&mut self, mut at: Instant) {
        if self.capture_started {
            let expected = self.callback_at + sample_duration(self.offset);
            let missing = at.saturating_duration_since(expected);
            if missing > Duration::from_millis(2) {
                self.position = self
                    .position
                    .saturating_add(super::outgoing::samples(missing));
                // Never splice a partial block across a device interruption.
                self.used = 0;
            } else if expected.saturating_duration_since(at) > Duration::from_millis(2) {
                // Some callback timestamps repeat/backtrack when buffers arrive
                // in a batch. Those samples still advance capture time. Moving
                // the baseline backward invents a gap when timestamps catch up,
                // repeatedly resets DSP/VAD and inflates the RTP sample clock.
                at = expected;
            }
        }
        self.capture_started = true;
        self.begin(at);
    }
    /// Time the first sample of this callback was captured / will be played.
    pub fn begin(&mut self, at: Instant) {
        self.callback_at = at;
        self.offset = 0;
    }

    /// Returns samples lost on overflow. A gap retains its sample position so
    /// the worker can reset adaptation instead of treating disjoint PCM as continuous.
    pub fn push(&mut self, sample: f32) -> u64 {
        if self.used == 0 {
            self.frame.at = self.callback_at + sample_duration(self.offset);
            self.frame.position = self.position;
        }
        self.frame.samples[self.used] = sample;
        self.used += 1;
        self.offset += 1;
        self.position = self.position.saturating_add(1);
        if self.used != SAMPLES {
            return 0;
        }
        let dropped = if self.queue.try_push(self.frame).is_err() {
            SAMPLES as u64
        } else {
            0
        };
        self.used = 0;
        dropped
    }
}

impl Reader {
    pub fn len(&self) -> usize {
        self.queue.occupied_len() + usize::from(self.pending.is_some())
    }

    pub fn pop(&mut self) -> Option<(Frame, bool)> {
        if let Some(pending) = self.pending.take() {
            return Some(pending);
        }
        let frame = self.queue.try_pop()?;
        let gap = frame.position != self.next;
        self.next = frame.position.saturating_add(SAMPLES as u64);
        Some((frame, gap))
    }

    pub fn retain(&mut self, frame: Frame) {
        debug_assert!(self.pending.is_none());
        *self.pending = Some((frame, true));
    }
}

pub fn sample_duration(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(RATE))
}

/// One device-local clock mapping per input stream. Recreated with the stream.
#[derive(Default)]
pub(super) struct CaptureClock {
    anchor: Option<(cpal::StreamInstant, Instant)>,
}

impl CaptureClock {
    pub fn map(&mut self, now: Instant, time: cpal::InputStreamTimestamp) -> Instant {
        // Only the first callback establishes the relation to Instant. Mapping
        // each callback against `now` folds scheduling/preemption into capture
        // time, inventing forward holes even when device samples are contiguous.
        // CPAL instants from this stream share a monotonic clock; origins from
        // different input/output streams must never be compared directly.
        let (device, local) = *self.anchor.get_or_insert((time.callback, now));
        if let Some(elapsed) = time.capture.checked_duration_since(device) {
            local.checked_add(elapsed).unwrap_or(now)
        } else {
            local
                .checked_sub(device.duration_since(time.capture))
                .unwrap_or(now)
        }
    }
}

/// Playback-reference timing is independent of the input stream's clock mapping.
pub fn playback_time(now: Instant, time: cpal::OutputStreamTimestamp) -> Instant {
    now.checked_add(time.playback.duration_since(time.callback))
        .unwrap_or(now)
}

pub fn delay_ms(render: Option<Instant>, capture: Instant) -> i32 {
    render
        .and_then(|at| at.checked_duration_since(capture))
        .unwrap_or_default()
        .as_millis()
        .min(500) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_scheduling_jitter_does_not_reset_capture() {
        let mut clock = CaptureClock::default();
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        let device = cpal::StreamInstant::new(100, 0);
        let mut delivered = at;
        for block in 0..6000 {
            let elapsed = sample_duration(block * SAMPLES);
            let timestamp = cpal::InputStreamTimestamp {
                capture: device + elapsed,
                callback: device + elapsed + Duration::from_millis(20),
            };
            // Timestamp collection and our callback can be separated by
            // preemption. Delayed callbacks then drain in quick succession.
            let delay = [0, 0, 4, 0, 25, 0, 0, 0, 45, 0, 0, 0, 0][block % 13];
            delivered = (delivered + Duration::from_micros(100))
                .max(at + elapsed + Duration::from_millis(20 + delay));
            writer.begin_capture(clock.map(delivered, timestamp));
            for _ in 0..SAMPLES {
                writer.push(0.1);
            }
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap, "scheduling became a capture gap at block {block}");
            assert_eq!(frame.position, (block * SAMPLES) as u64);
            assert_eq!(frame.at, at + elapsed);
        }
    }

    #[test]
    fn capture_clock_preserves_real_holes_and_age_of_delayed_audio() {
        let mut clock = CaptureClock::default();
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        let device = cpal::StreamInstant::new(100, 0);
        for (sample, delay) in [(0, 0), (480, 300), (960 + 240, 300)] {
            let elapsed = sample_duration(sample);
            let timestamp = cpal::InputStreamTimestamp {
                capture: device + elapsed,
                callback: device + elapsed + Duration::from_millis(20),
            };
            let now = at + elapsed + Duration::from_millis(20 + delay);
            writer.begin_capture(clock.map(now, timestamp));
            for _ in 0..SAMPLES {
                writer.push(0.1);
            }
            let (frame, gap) = reader.pop().unwrap();
            assert_eq!(gap, sample == 1200);
            assert_eq!(frame.position, sample as u64);
            assert_eq!(frame.at, at + elapsed);
            if delay > 0 {
                assert!(now.duration_since(frame.at) > super::super::outgoing::MAX_PACKET_AGE);
            }
        }
        // A replacement stream has its own origin and must not inherit this
        // stream's clock. Its first buffer can precede the callback anchor.
        let mut replacement = CaptureClock::default();
        let later = at + Duration::from_secs(20);
        assert_eq!(
            replacement.map(
                later,
                cpal::InputStreamTimestamp {
                    capture: cpal::StreamInstant::new(0, 0),
                    callback: cpal::StreamInstant::new(0, 20_000_000),
                },
            ),
            later - Duration::from_millis(20)
        );
    }

    #[test]
    fn capture_gap_discards_partial_block_and_keeps_missing_samples_in_position() {
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        writer.begin_capture(at);
        for _ in 0..617 {
            writer.push(0.1);
        }
        assert_eq!(reader.pop().unwrap().0.position, 0);
        // 137 partial samples followed by a 5 ms hole.
        writer.begin_capture(at + sample_duration(617 + 240));
        for _ in 0..480 {
            writer.push(0.2);
        }
        let (frame, gap) = reader.pop().unwrap();
        assert!(gap);
        assert_eq!(frame.position, 857);
        assert_eq!(frame.samples, [0.2; 480]);
        assert_eq!(frame.at, at + sample_duration(857));
        writer.begin_capture(at + sample_duration(1337));
        for _ in 0..480 {
            writer.push(0.3);
        }
        let (frame, gap) = reader.pop().unwrap();
        assert!(!gap);
        assert_eq!(frame.position, 1337);
    }

    #[test]
    fn capture_timestamp_jitter_does_not_change_sample_clock() {
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        for block in 0..100 {
            let jitter = if block % 2 == 0 { 0 } else { 48 };
            writer.begin_capture(at + sample_duration(block * 480 + jitter));
            for _ in 0..480 {
                writer.push(0.0);
            }
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap);
            assert_eq!(frame.position, block as u64 * 480);
        }
    }

    #[test]
    fn repeated_capture_timestamps_do_not_create_gaps_or_rewind_frames() {
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        // The recording delivered pairs of 10 ms blocks at almost the same
        // timestamp, then advanced about 20 ms. Include a small backward step.
        for block in 0..1600 {
            let observed = at + sample_duration(block / 2 * 960);
            let observed = if block % 2 == 1 {
                observed - sample_duration(8)
            } else {
                observed
            };
            writer.begin_capture(observed);
            for _ in 0..SAMPLES {
                writer.push(0.1);
            }
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap, "false capture gap at block {block}");
            assert_eq!(frame.position, (block * SAMPLES) as u64);
            assert_eq!(frame.at, at + sample_duration(block * SAMPLES));
        }
    }

    #[test]
    fn real_capture_hole_after_repeated_timestamp_still_discards_partial_audio() {
        let (mut writer, mut reader) = queue();
        let at = Instant::now();
        for _ in 0..2 {
            writer.begin_capture(at);
            for _ in 0..617 {
                writer.push(0.1);
            }
        }
        // Two callbacks supplied 1234 samples despite sharing a timestamp.
        // Only the following 5 ms are missing, not either callback's samples.
        let resumed = at + sample_duration(1234 + 240);
        writer.begin_capture(resumed);
        for _ in 0..SAMPLES {
            writer.push(0.2);
        }
        assert!(!reader.pop().unwrap().1);
        assert!(!reader.pop().unwrap().1);
        let (frame, gap) = reader.pop().unwrap();
        assert!(gap);
        assert_eq!(frame.position, 1474);
        assert_eq!(frame.at, resumed);
        assert_eq!(frame.samples, [0.2; SAMPLES]);
    }

    #[test]
    fn capture_clock_skew_does_not_accumulate_into_false_discontinuities() {
        // One sample per callback is more drift than expected from real clocks.
        // Small adjustments must still follow the device in both directions;
        // forcing every callback onto a rigid 48 kHz wall clock would diverge.
        for step in [479, 481] {
            let (mut writer, mut reader) = queue();
            let at = Instant::now();
            for block in 0..10_000 {
                let observed = at + sample_duration(block * step);
                writer.begin_capture(observed);
                for _ in 0..SAMPLES {
                    writer.push(0.0);
                }
                let (frame, gap) = reader.pop().unwrap();
                assert!(!gap, "step={step} block={block}");
                assert_eq!(frame.position, (block * SAMPLES) as u64);
                assert_eq!(frame.at, observed);
            }
        }
    }

    #[test]
    fn large_and_partial_callbacks_preserve_samples_and_capture_times() {
        let (mut writer, mut reader) = queue();
        let start = Instant::now();
        // A 100 ms callback used to lose 40 ms at the worker's 60 ms trim.
        let sizes = [137, 4800, 343];
        let mut total = 0;
        for size in sizes {
            writer.begin_capture(start + sample_duration(total));
            for i in total..total + size {
                assert_eq!(writer.push(i as f32), 0);
            }
            total += size;
        }
        assert_eq!(reader.len(), total / SAMPLES);
        for block in 0..total / SAMPLES {
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap);
            assert_eq!(frame.position, (block * SAMPLES) as u64);
            let expected = start + sample_duration(block * SAMPLES);
            assert!(frame.at.max(expected) - frame.at.min(expected) < Duration::from_micros(1));
            for (i, sample) in frame.samples.iter().enumerate() {
                assert_eq!(*sample, (block * SAMPLES + i) as f32);
            }
        }
    }

    #[test]
    fn overflow_is_bounded_and_marks_the_next_delivered_frame() {
        let (mut writer, mut reader) = queue();
        let dropped: u64 = (0..CAPACITY + SAMPLES).map(|_| writer.push(0.0)).sum();
        assert_eq!(dropped, SAMPLES as u64);
        for _ in 0..BLOCKS {
            assert!(!reader.pop().unwrap().1);
        }
        for _ in 0..SAMPLES {
            writer.push(0.0);
        }
        assert!(reader.pop().unwrap().1);
    }

    #[test]
    fn delay_tracks_device_timing_and_is_bounded() {
        let now = Instant::now();
        assert_eq!(delay_ms(Some(now + Duration::from_millis(137)), now), 137);
        assert_eq!(delay_ms(Some(now + Duration::from_secs(2)), now), 500);
        assert_eq!(delay_ms(Some(now), now + Duration::from_millis(10)), 0);
        assert_eq!(delay_ms(None, now), 0);
    }
}
