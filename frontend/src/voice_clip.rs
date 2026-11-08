//! Platform-independent voice-message rules: clock labels, waveform fitting,
//! playback speeds, recorder gesture thresholds and a pitch-preserving
//! time-stretcher used by the native player. No allocation after construction
//! in the stretcher's steady state; it runs on a worker thread, never in an
//! audio callback.
use thiscord_shared::voice_message::{WAVEFORM_BARS, decode_waveform};

/// Playback speeds offered by the player, in cycling order.
pub const SPEEDS: [f32; 3] = [1.0, 1.5, 2.0];

pub fn next_speed(current: f32) -> f32 {
    let index = SPEEDS
        .iter()
        .position(|s| (s - current).abs() < 0.01)
        .unwrap_or(0);
    SPEEDS[(index + 1) % SPEEDS.len()]
}

pub fn speed_label(speed: f32) -> &'static str {
    if (speed - 1.5).abs() < 0.01 {
        "1.5×"
    } else if (speed - 2.0).abs() < 0.01 {
        "2×"
    } else {
        "1×"
    }
}

/// `m:ss`, rounding down; durations of an hour or more never occur.
pub fn clock(milliseconds: u32) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Bars (0–255) for display, resampled to `count` from the wire waveform.
/// Invalid or missing waveforms render as a gentle flat line.
pub fn bars(waveform: &str, count: usize) -> Vec<u8> {
    let source = decode_waveform(waveform).unwrap_or_else(|| vec![48; WAVEFORM_BARS]);
    resample(&source, count)
}

pub fn resample(source: &[u8], count: usize) -> Vec<u8> {
    if source.is_empty() || count == 0 {
        return vec![0; count];
    }
    (0..count)
        .map(|i| {
            let start = i * source.len() / count;
            let end = ((i + 1) * source.len() / count).max(start + 1);
            *source[start..end.min(source.len())]
                .iter()
                .max()
                .unwrap_or(&0)
        })
        .collect()
}

/// Live recorder level (linear peak or RMS 0–1) to a bar height (0–255) on
/// the same 50 dB scale as stored waveforms.
pub fn level_bar(level: f32) -> u8 {
    let db = 20.0 * level.max(1e-5).log10();
    (((db + 50.0) / 50.0).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Seek target for keyboard steps on the waveform slider.
pub fn step(position: u32, duration: u32, key: &str) -> Option<u32> {
    let jump = 5_000;
    Some(match key {
        "ArrowRight" | "ArrowUp" => position.saturating_add(jump).min(duration),
        "ArrowLeft" | "ArrowDown" => position.saturating_sub(jump),
        "PageUp" => position.saturating_add(jump * 3).min(duration),
        "PageDown" => position.saturating_sub(jump * 3),
        "Home" => 0,
        "End" => duration,
        _ => return None,
    })
}

/// What a press-and-hold gesture on the microphone button means on release
/// or while moving. Distances are CSS pixels from the press point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Still holding: release sends.
    Holding,
    /// Slid left far enough: release (or reaching it) discards.
    Cancel,
    /// Slid up far enough: keep recording hands-free.
    Lock,
}
pub const CANCEL_DISTANCE: f64 = 96.0;
pub const LOCK_DISTANCE: f64 = 64.0;
/// Presses shorter than this toggle a hands-free recording instead.
pub const TAP_MILLISECONDS: f64 = 280.0;

pub fn gesture(dx: f64, dy: f64) -> Gesture {
    if -dy >= LOCK_DISTANCE && -dy > -dx {
        Gesture::Lock
    } else if -dx >= CANCEL_DISTANCE {
        Gesture::Cancel
    } else {
        Gesture::Holding
    }
}

/// WSOLA time-stretcher for mono 48 kHz speech: overlap-adds 20 ms Hann
/// frames at half-frame output hops, choosing each input frame within ±5 ms
/// to best continue the previous one. Pitch is preserved; 1× passes through.
pub struct Stretcher {
    speed: f32,
    input: Vec<f32>,
    /// Fractional analysis position within `input`.
    position: f64,
    /// Start of the previous analysis frame, or None before the first frame.
    previous: Option<usize>,
    overlap: Vec<f32>,
    window: Vec<f32>,
}
const FRAME: usize = 960;
const HOP: usize = FRAME / 2;
const TOLERANCE: usize = 240;

impl Stretcher {
    pub fn new(speed: f32) -> Self {
        let window = (0..FRAME)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FRAME as f32).cos())
            .collect();
        Self {
            speed: speed.clamp(0.5, 3.0),
            input: Vec::with_capacity(FRAME * 8),
            position: 0.0,
            previous: None,
            overlap: vec![0.0; FRAME],
            window,
        }
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Drops buffered audio (seek or speed change).
    pub fn reset(&mut self, speed: f32) {
        self.speed = speed.clamp(0.5, 3.0);
        self.input.clear();
        self.position = 0.0;
        self.previous = None;
        self.overlap.fill(0.0);
    }

    fn passthrough(&self) -> bool {
        (self.speed - 1.0).abs() < 0.01
    }

    /// Input samples consumed per output sample.
    pub fn ratio(&self) -> f32 {
        self.speed
    }

    /// Appends input and writes every output sample that is now complete.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough() {
            output.extend_from_slice(input);
            return;
        }
        self.input.extend_from_slice(input);
        let analysis = HOP as f64 * f64::from(self.speed);
        loop {
            let nominal = self.position.round() as usize;
            // Need the frame plus the search window after the nominal start.
            if nominal + TOLERANCE + FRAME > self.input.len() {
                break;
            }
            let start = match self.previous {
                None => nominal,
                Some(previous) => {
                    let natural = previous + HOP;
                    let low = nominal.saturating_sub(TOLERANCE);
                    let high = nominal + TOLERANCE;
                    let template = &self.input[natural..natural + HOP];
                    let mut best = (f32::MIN, nominal);
                    // Step 2 halves the search cost with no audible difference.
                    for candidate in (low..=high).step_by(2) {
                        let span = &self.input[candidate..candidate + HOP];
                        let score: f32 = template
                            .iter()
                            .zip(span)
                            .step_by(2)
                            .map(|(a, b)| a * b)
                            .sum();
                        if score > best.0 {
                            best = (score, candidate);
                        }
                    }
                    best.1
                }
            };
            for (i, sample) in self.input[start..start + FRAME].iter().enumerate() {
                self.overlap[i] += sample * self.window[i];
            }
            output.extend_from_slice(&self.overlap[..HOP]);
            self.overlap.copy_within(HOP.., 0);
            self.overlap[FRAME - HOP..].fill(0.0);
            self.previous = Some(start);
            self.position += analysis;
            // Drop consumed input while keeping the template and search range.
            let keep_from = self
                .previous
                .unwrap_or(0)
                .min(self.position as usize)
                .saturating_sub(TOLERANCE);
            if keep_from > FRAME * 4 {
                self.input.drain(..keep_from);
                self.position -= keep_from as f64;
                self.previous = self.previous.map(|p| p - keep_from);
            }
        }
    }

    /// Flushes the remaining overlap at the end of a clip.
    pub fn finish(&mut self, output: &mut Vec<f32>) {
        if self.passthrough() {
            return;
        }
        let rest = self.input.len().saturating_sub(self.position as usize);
        let tail = (rest as f32 / self.speed) as usize;
        output.extend_from_slice(&self.overlap[..HOP.min(FRAME)]);
        output.extend(std::iter::repeat_n(0.0, tail.min(FRAME * 2)));
        self.reset(self.speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::voice_message::encode_waveform;

    #[test]
    fn labels_speeds_and_keyboard_steps() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(7_999), "0:07");
        assert_eq!(clock(65_000), "1:05");
        assert_eq!(clock(300_000), "5:00");
        assert_eq!(next_speed(1.0), 1.5);
        assert_eq!(next_speed(1.5), 2.0);
        assert_eq!(next_speed(2.0), 1.0);
        assert_eq!(next_speed(7.0), 1.5);
        assert_eq!(speed_label(1.5), "1.5×");
        assert_eq!(step(1_000, 12_000, "ArrowRight"), Some(6_000));
        assert_eq!(step(1_000, 12_000, "ArrowLeft"), Some(0));
        assert_eq!(step(11_000, 12_000, "PageUp"), Some(12_000));
        assert_eq!(step(4_000, 12_000, "End"), Some(12_000));
        assert_eq!(step(4_000, 12_000, "x"), None);
    }

    #[test]
    fn waveforms_resample_and_fall_back_to_flat() {
        let source: Vec<u8> = (0..WAVEFORM_BARS as u8).collect();
        let wire = encode_waveform(&source);
        assert_eq!(bars(&wire, 64), source);
        let half = bars(&wire, 32);
        assert_eq!(half.len(), 32);
        assert_eq!(half[0], 1);
        assert_eq!(bars(&wire, 128).len(), 128);
        assert_eq!(bars("garbage", 10), vec![48; 10]);
        assert_eq!(level_bar(1.0), 255);
        assert_eq!(level_bar(0.0), 0);
        assert!(level_bar(0.05) > 100);
    }

    #[test]
    fn hold_gestures_lock_upwards_and_cancel_leftwards() {
        assert_eq!(gesture(0.0, 0.0), Gesture::Holding);
        assert_eq!(gesture(-40.0, -10.0), Gesture::Holding);
        assert_eq!(gesture(-120.0, -5.0), Gesture::Cancel);
        assert_eq!(gesture(-10.0, -80.0), Gesture::Lock);
        // A diagonal swipe resolves to its dominant direction.
        assert_eq!(gesture(-150.0, -70.0), Gesture::Cancel);
        assert_eq!(gesture(30.0, 40.0), Gesture::Holding);
    }

    fn sine(frequency: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (2.0 * std::f32::consts::PI * frequency * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    fn crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count()
    }

    #[test]
    fn stretching_shortens_without_changing_pitch() {
        let input = sine(220.0, 48_000 * 2);
        for speed in [1.5f32, 2.0] {
            let mut stretcher = Stretcher::new(speed);
            let mut output = Vec::new();
            for chunk in input.chunks(480) {
                stretcher.process(chunk, &mut output);
            }
            let expected = input.len() as f32 / speed;
            assert!(
                (output.len() as f32 - expected).abs() < 48_000.0 * 0.05,
                "{speed}: {} vs {expected}",
                output.len()
            );
            // Same frequency: crossings per sample stay at 220 per second.
            let steady = &output[4_800..output.len() - 4_800];
            let rate = crossings(steady) as f32 * 48_000.0 / steady.len() as f32;
            assert!((rate - 220.0).abs() < 6.0, "{speed}: {rate} Hz");
            // Overlap-add keeps the level (no comb cancellation dips).
            let peak = steady.iter().fold(0f32, |a, b| a.max(b.abs()));
            assert!((0.4..0.6).contains(&peak), "{speed}: peak {peak}");
        }
        let mut same = Stretcher::new(1.0);
        let mut output = Vec::new();
        same.process(&input[..960], &mut output);
        assert_eq!(output, &input[..960]);
    }

    #[test]
    fn stretcher_buffers_stay_bounded_and_reset() {
        let mut stretcher = Stretcher::new(2.0);
        let mut output = Vec::new();
        for _ in 0..2_000 {
            stretcher.process(&[0.1; 480], &mut output);
            output.clear();
            assert!(stretcher.input.len() < FRAME * 8);
        }
        stretcher.reset(1.5);
        assert!(stretcher.input.is_empty() && stretcher.previous.is_none());
        assert_eq!(stretcher.speed(), 1.5);
    }
}
