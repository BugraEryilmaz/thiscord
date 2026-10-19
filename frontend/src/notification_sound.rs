//! One sound palette for browser previews, desktop alerts and the voice mixer.
//! Render outside audio callbacks; playback only reads these bounded PCM buffers.
use thiscord_shared::notifications::Kind;

pub const PEAK: f32 = 0.12;
// Preserve the native mixer's bit positions; do not use the wire enum's ordering.
pub const MIXER_KINDS: [Kind; 6] = [
    Kind::VoiceJoin,
    Kind::VoiceLeave,
    Kind::DirectMessage,
    Kind::GuildMessage,
    Kind::IncomingCall,
    Kind::Update,
];
pub fn mixer_index(kind: Kind) -> usize {
    MIXER_KINDS.iter().position(|k| *k == kind).unwrap()
}
pub fn description(kind: Kind) -> &'static str {
    match kind {
        Kind::DirectMessage => "Bright double chime",
        Kind::GuildMessage => "Short, low knock",
        Kind::IncomingCall => "Two pairs of ringing tones",
        Kind::VoiceJoin => "Smooth rising tone",
        Kind::VoiceLeave => "Smooth falling tone",
        Kind::Update => "Four-note celebration chime",
    }
}
#[derive(Clone, Copy)]
enum Timbre {
    Bell,
    Knock,
    Ring,
    Soft,
}
#[derive(Clone, Copy)]
struct Note {
    start_ms: usize,
    length_ms: usize,
    from: f32,
    to: f32,
    timbre: Timbre,
}
impl Note {
    const fn tone(start_ms: usize, length_ms: usize, hz: f32, timbre: Timbre) -> Self {
        Self {
            start_ms,
            length_ms,
            from: hz,
            to: hz,
            timbre,
        }
    }
}
fn notes(kind: Kind) -> &'static [Note] {
    use Timbre::*;
    match kind {
        Kind::DirectMessage => {
            const {
                &[
                    Note::tone(0, 110, 880.0, Bell),
                    Note::tone(160, 240, 1320.0, Bell),
                ]
            }
        }
        Kind::GuildMessage => const { &[Note::tone(0, 130, 360.0, Knock)] },
        Kind::IncomingCall => {
            const {
                &[
                    Note::tone(0, 180, 600.0, Ring),
                    Note::tone(260, 180, 600.0, Ring),
                    Note::tone(650, 180, 600.0, Ring),
                    Note::tone(910, 180, 600.0, Ring),
                ]
            }
        }
        Kind::VoiceJoin => &[Note {
            start_ms: 0,
            length_ms: 260,
            from: 380.0,
            to: 920.0,
            timbre: Soft,
        }],
        Kind::VoiceLeave => &[Note {
            start_ms: 0,
            length_ms: 360,
            from: 720.0,
            to: 260.0,
            timbre: Soft,
        }],
        Kind::Update => {
            const {
                &[
                    Note::tone(0, 100, 523.25, Bell),
                    Note::tone(140, 100, 659.25, Bell),
                    Note::tone(280, 100, 784.0, Bell),
                    Note::tone(420, 300, 1046.5, Bell),
                ]
            }
        }
    }
}
pub fn duration_ms(kind: Kind) -> usize {
    notes(kind)
        .iter()
        .map(|n| n.start_ms + n.length_ms)
        .max()
        .unwrap()
}
pub fn render(kind: Kind, sample_rate: u32) -> Vec<f32> {
    let rate = sample_rate as usize;
    let mut samples = vec![0.0; duration_ms(kind) * rate / 1000];
    for note in notes(kind) {
        let start = note.start_ms * rate / 1000;
        let end = (note.start_ms + note.length_ms) * rate / 1000;
        let length = end - start;
        for (n, sample) in samples[start..end].iter_mut().enumerate() {
            if n == 0 || n + 1 == length {
                continue;
            }
            let t = n as f32 / sample_rate as f32;
            let duration = note.length_ms as f32 / 1000.0;
            // Integrate the frequency ramp for a continuous, click-free sweep.
            let phase = std::f32::consts::TAU
                * (note.from * t + (note.to - note.from) * t * t / (2.0 * duration));
            let progress = n as f32 / (length - 1) as f32;
            let attack = (t / 0.008).min(1.0);
            let release = ((length - 1 - n) as f32 / sample_rate as f32 / 0.025).min(1.0);
            let edge = (std::f32::consts::FRAC_PI_2 * attack.min(release))
                .sin()
                .powi(2);
            let (wave, decay) = match note.timbre {
                Timbre::Bell => (
                    0.75 * phase.sin() + 0.25 * (phase * 2.0).sin(),
                    (-3.0 * progress).exp(),
                ),
                Timbre::Knock => (
                    0.65 * phase.sin() + 0.25 * (phase * 2.0).sin() + 0.1 * (phase * 3.0).sin(),
                    (-7.0 * progress).exp(),
                ),
                Timbre::Ring => (0.55 * phase.sin() + 0.45 * (phase * (4.0 / 3.0)).sin(), 1.0),
                Timbre::Soft => (phase.sin(), 0.75),
            };
            *sample += PEAK * edge * decay * wave;
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_has_bounded_level_duration_and_silent_edges_at_device_rates() {
        for rate in [8000, 16000, 44100, 48000, 96000, 192000] {
            for kind in Kind::ALL {
                let samples = render(kind, rate);
                assert_eq!(samples.len(), duration_ms(kind) * rate as usize / 1000);
                assert!(samples.len() < rate as usize * 2);
                assert!(samples.iter().all(|s| s.is_finite() && s.abs() <= PEAK));
                assert!(samples.iter().any(|s| s.abs() > 0.02));
                for note in notes(kind) {
                    assert_eq!(samples[note.start_ms * rate as usize / 1000], 0.0);
                    assert_eq!(
                        samples[(note.start_ms + note.length_ms) * rate as usize / 1000 - 1],
                        0.0
                    );
                }
            }
        }
    }
    #[test]
    fn audible_cadences_are_distinct_and_ringing_contains_a_long_pause() {
        // Measure actual non-silent runs, rather than merely comparing PCM bytes.
        let runs = |kind| {
            let samples = render(kind, 48000);
            let mut runs = vec![];
            for (index, chunk) in samples.chunks(480).enumerate() {
                let audible = chunk.iter().any(|s| s.abs() > 0.002);
                if audible && runs.last().is_none_or(|(_, end)| *end != index) {
                    runs.push((index, index + 1));
                } else if audible {
                    runs.last_mut().unwrap().1 = index + 1;
                }
            }
            runs
        };
        assert_eq!(runs(Kind::DirectMessage).len(), 2);
        assert_eq!(runs(Kind::GuildMessage).len(), 1);
        let ringing = runs(Kind::IncomingCall);
        assert_eq!(ringing.len(), 4);
        assert!(ringing[2].0 - ringing[1].1 >= 20);
        assert_eq!(runs(Kind::Update).len(), 4);
        // Rising and falling voice sweeps must have opposite pitch direction.
        let crossings = |kind, start| {
            render(kind, 48000)[start..start + 2400]
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count()
        };
        assert!(crossings(Kind::VoiceJoin, 8000) > crossings(Kind::VoiceJoin, 1000));
        assert!(crossings(Kind::VoiceLeave, 10000) < crossings(Kind::VoiceLeave, 1000));
    }
}
