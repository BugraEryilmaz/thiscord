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
    sequence: u64,
}

pub struct Writer {
    queue: HeapProd<Frame>,
    frame: Frame,
    used: usize,
    callback_at: Instant,
    offset: usize,
}

pub struct Reader {
    queue: HeapCons<Frame>,
    next: u64,
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
                sequence: 0,
            },
            used: 0,
            callback_at: now,
            offset: 0,
        },
        Reader {
            queue: consumer,
            next: 0,
        },
    )
}

impl Writer {
    /// Time the first sample of this callback was captured / will be played.
    pub fn begin(&mut self, at: Instant) {
        self.callback_at = at;
        self.offset = 0;
    }

    /// Returns samples lost on overflow. A gap retains its sequence number so
    /// the worker can reset adaptation instead of treating disjoint PCM as continuous.
    pub fn push(&mut self, sample: f32) -> u64 {
        if self.used == 0 {
            self.frame.at = self.callback_at + sample_duration(self.offset);
        }
        self.frame.samples[self.used] = sample;
        self.used += 1;
        self.offset += 1;
        if self.used != SAMPLES {
            return 0;
        }
        let dropped = if self.queue.try_push(self.frame).is_err() {
            SAMPLES as u64
        } else {
            0
        };
        self.used = 0;
        self.frame.sequence = self.frame.sequence.wrapping_add(1);
        dropped
    }
}

impl Reader {
    pub fn len(&self) -> usize {
        self.queue.occupied_len()
    }

    pub fn pop(&mut self) -> Option<(Frame, bool)> {
        let frame = self.queue.try_pop()?;
        let gap = frame.sequence != self.next;
        self.next = frame.sequence.wrapping_add(1);
        Some((frame, gap))
    }
}

pub fn sample_duration(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(RATE))
}

/// CPAL times are device-local. Only use durations within one callback's clock,
/// then map onto Instant so independently clocked input/output devices can compare.
pub fn capture_time(now: Instant, time: cpal::InputStreamTimestamp) -> Instant {
    now.checked_sub(time.callback.duration_since(time.capture))
        .unwrap_or(now)
}

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
    fn large_and_partial_callbacks_preserve_samples_and_capture_times() {
        let (mut writer, mut reader) = queue();
        let start = Instant::now();
        // A 100 ms callback used to lose 40 ms at the worker's 60 ms trim.
        let sizes = [137, 4800, 343];
        let mut total = 0;
        for size in sizes {
            writer.begin(start + sample_duration(total));
            for i in total..total + size {
                assert_eq!(writer.push(i as f32), 0);
            }
            total += size;
        }
        assert_eq!(reader.len(), total / SAMPLES);
        for block in 0..total / SAMPLES {
            let (frame, gap) = reader.pop().unwrap();
            assert!(!gap);
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
