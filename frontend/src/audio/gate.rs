//! Worker-only energy activation. No speech/person classification is claimed.
//! One input packet advances exactly 20 ms, independent of worker scheduling.
use std::time::{Duration, Instant};
use thiscord_shared::audio::TransmitMode;

pub(super) const PRE_ROLL: usize = 3;
pub(super) const DELAY: Duration = Duration::from_millis(PRE_ROLL as u64 * 20);
const ATTACK: usize = 2;
const RELEASE: usize = 10;

/// RMS of each denoised, statically boosted 10 ms half, before optional automatic gain.
#[derive(Clone, Copy, Default)]
pub(super) struct Evidence(pub [f32; 2]);
impl Evidence {
    pub fn rms(block: &[f32; 480]) -> f32 {
        (block.iter().map(|v| v * v).sum::<f32>() / 480.0).sqrt()
    }
}

#[derive(Clone, Copy)]
struct Retained {
    pcm: [f32; 960],
    active: bool,
    valid: bool,
}
const SILENCE: Retained = Retained {
    pcm: [0.0; 960],
    active: false,
    valid: false,
};

pub(super) struct Output {
    pub pcm: [f32; 960],
    pub active: bool,
}

pub(super) struct Gate {
    ring: [Retained; PRE_ROLL],
    head: usize,
    attack: usize,
    release: usize,
    open: bool,
    last: Option<Instant>,
    mode: TransmitMode,
    threshold: f32,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            ring: [SILENCE; PRE_ROLL],
            head: 0,
            attack: 0,
            release: 0,
            open: false,
            last: None,
            mode: TransmitMode::VoiceActivity,
            threshold: 0.015,
        }
    }
}
impl Gate {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn discontinuous(&self, at: Instant, gap: bool) -> bool {
        gap || self
            .last
            .is_some_and(|last| at <= last || at.duration_since(last) > Duration::from_millis(40))
    }

    /// `allowed` includes mute, deafen, stop and connection permission; PTT key
    /// state is separate. Callers invalidate on *every* control boundary, even
    /// when restriction and relaxation happen between two processing ticks.
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        pcm: [f32; 960],
        evidence: Evidence,
        at: Instant,
        gap: bool,
        mode: TransmitMode,
        threshold: f32,
        allowed: bool,
        pressed: bool,
    ) -> Output {
        // Timestamp tolerance permits device-clock jitter; sequence gaps are
        // authoritative. Never span backward timestamps or a >40 ms gap.
        if self.discontinuous(at, gap) || mode != self.mode || threshold != self.threshold {
            self.reset();
        }
        self.mode = mode;
        self.threshold = threshold;
        self.last = Some(at);
        if !allowed
            || !threshold.is_finite()
            || !(0.001..=0.5).contains(&threshold)
            || evidence.0.iter().any(|v| !v.is_finite())
            || pcm.iter().any(|v| !v.is_finite())
        {
            self.reset();
            return Output {
                pcm: SILENCE.pcm,
                active: false,
            };
        }
        if mode == TransmitMode::PushToTalk {
            // No automatic threshold, lookbehind, added delay or release tail.
            self.reset();
            return Output {
                pcm: if pressed { pcm } else { SILENCE.pcm },
                active: pressed,
            };
        }

        // Preserve the existing slider/default scale for a sustained sinusoid:
        // threshold is peak-equivalent RMS. Both halves must qualify to attack.
        let opening = threshold * std::f32::consts::FRAC_1_SQRT_2;
        let closing = opening * 0.5;
        if self.open {
            if evidence.0.iter().any(|v| *v >= closing) {
                self.release = RELEASE;
            } else {
                if self.release == 0 {
                    self.open = false;
                } else {
                    self.release -= 1;
                }
            }
        } else if evidence.0.iter().all(|v| *v >= opening) {
            self.attack += 1;
            if self.attack == ATTACK {
                self.open = true;
                self.release = RELEASE;
                // The oldest eligible packet has not yet been emitted. Mark
                // the fixed lookbehind, never rewind the output timeline.
                for frame in &mut self.ring {
                    frame.active = frame.valid;
                }
            }
        } else {
            self.attack = 0;
        }
        if self.open {
            self.attack = 0;
        }
        let due = std::mem::replace(
            &mut self.ring[self.head],
            Retained {
                pcm,
                active: self.open,
                valid: true,
            },
        );
        self.head = (self.head + 1) % PRE_ROLL;
        Output {
            pcm: if due.active { due.pcm } else { SILENCE.pcm },
            active: due.active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: f32 = 0.015;
    struct Harness {
        gate: Gate,
        start: Instant,
        packet: usize,
    }
    impl Harness {
        fn new() -> Self {
            Self {
                gate: Gate::default(),
                start: Instant::now(),
                packet: 0,
            }
        }
        fn step(&mut self, level: f32) -> Output {
            let pcm = [self.packet as f32 / 1000.0 + 0.001; 960];
            let out = self.gate.process(
                pcm,
                Evidence([level; 2]),
                self.start + Duration::from_millis(self.packet as u64 * 20),
                false,
                TransmitMode::VoiceActivity,
                THRESHOLD,
                true,
                false,
            );
            self.packet += 1;
            out
        }
    }

    #[test]
    fn attack_preroll_capacity_continuity_and_exact_added_delay() {
        let mut h = Harness::new();
        // Two quiet onset packets, then 40 ms above the opening threshold.
        assert!(!h.step(0.003).active);
        assert!(!h.step(0.003).active);
        assert!(!h.step(0.03).active);
        assert!(!h.gate.open);
        let first = h.step(0.03);
        assert!(h.gate.open);
        assert!(first.active);
        assert_eq!(first.pcm, [0.001; 960]);
        for source in 1..200 {
            let output = h.step(0.03);
            assert_eq!(output.pcm, [source as f32 / 1000.0 + 0.001; 960]);
            assert_eq!(h.packet - 1 - source, PRE_ROLL);
        }
        assert_eq!(PRE_ROLL * 20, 60);
        assert!(std::mem::size_of::<Gate>() < 12_000);
    }

    #[test]
    fn exact_release_and_quiet_consonants_and_sentence_pauses() {
        let mut h = Harness::new();
        h.step(0.03);
        h.step(0.03);
        // Below opening, above closing: quiet consonants never chatter closed.
        for _ in 0..40 {
            h.step(0.007);
            assert!(h.gate.open);
        }
        // A 160 ms pause and a quiet syllable refresh the release.
        for _ in 0..8 {
            h.step(0.0);
            assert!(h.gate.open);
        }
        h.step(0.007);
        for _ in 0..10 {
            h.step(0.002);
            assert!(h.gate.open);
        }
        h.step(0.002);
        assert!(!h.gate.open);
        // Already permitted trailing packets drain at ordinary cadence.
        for _ in 0..2 {
            assert!(h.step(0.0).active);
        }
        assert!(!h.step(0.0).active);
    }

    #[test]
    fn threshold_boundary_chatter_requires_sustained_attack() {
        let mut h = Harness::new();
        let opening = THRESHOLD * std::f32::consts::FRAC_1_SQRT_2;
        for _ in 0..100 {
            h.step(opening * 1.01);
            h.step(opening * 0.99);
            assert!(!h.gate.open);
        }
        h.step(opening);
        h.step(opening);
        assert!(h.gate.open);
        for _ in 0..100 {
            h.step(opening * 0.99);
            assert!(h.gate.open);
        }
    }

    #[test]
    fn restrictions_ptt_release_gaps_modes_and_resets_never_replay_history() {
        for boundary in 0..8 {
            let mut h = Harness::new();
            for _ in 0..5 {
                h.step(0.03);
            }
            let at = h.start + Duration::from_millis(100);
            match boundary {
                // The same deny input represents mute, deafen, stop, permission.
                0..=3 => {
                    let out = h.gate.process(
                        [0.9; 960],
                        Evidence([0.9; 2]),
                        at,
                        false,
                        TransmitMode::VoiceActivity,
                        THRESHOLD,
                        false,
                        false,
                    );
                    assert!(!out.active);
                }
                4 => h.gate.reset(),
                5 => {
                    h.gate.process(
                        [0.0; 960],
                        Evidence::default(),
                        at,
                        true,
                        TransmitMode::VoiceActivity,
                        THRESHOLD,
                        true,
                        false,
                    );
                }
                6 => {
                    h.gate.process(
                        [0.0; 960],
                        Evidence::default(),
                        at + Duration::from_secs(1),
                        false,
                        TransmitMode::VoiceActivity,
                        THRESHOLD,
                        true,
                        false,
                    );
                }
                7 => {
                    let out = h.gate.process(
                        [0.7; 960],
                        Evidence::default(),
                        at,
                        false,
                        TransmitMode::PushToTalk,
                        THRESHOLD,
                        true,
                        true,
                    );
                    assert_eq!(out.pcm, [0.7; 960]);
                    let out = h.gate.process(
                        [0.9; 960],
                        Evidence([0.9; 2]),
                        at + Duration::from_millis(20),
                        false,
                        TransmitMode::PushToTalk,
                        THRESHOLD,
                        true,
                        false,
                    );
                    assert!(!out.active);
                }
                _ => unreachable!(),
            }
            h.packet += 2;
            for _ in 0..30 {
                assert_eq!(h.step(0.0).pcm, [0.0; 960]);
            }
        }
    }

    #[test]
    fn ptt_is_immediate_even_below_threshold_and_has_no_prepress_or_tail() {
        let mut gate = Gate::default();
        let start = Instant::now();
        for index in 0..20 {
            let pressed = (5..10).contains(&index);
            let pcm = [if pressed { 0.00001 } else { 0.8 }; 960];
            let output = gate.process(
                pcm,
                Evidence([0.0; 2]),
                start + Duration::from_millis(index * 20),
                false,
                TransmitMode::PushToTalk,
                0.5,
                true,
                pressed,
            );
            assert_eq!(output.active, pressed);
            assert_eq!(output.pcm, if pressed { pcm } else { [0.0; 960] });
        }
    }

    #[test]
    fn invalid_evidence_audio_and_settings_fail_closed_and_clear_preroll() {
        for (pcm, evidence, threshold) in [
            ([0.3; 960], Evidence([f32::NAN; 2]), THRESHOLD),
            ([f32::INFINITY; 960], Evidence([0.3; 2]), THRESHOLD),
            ([0.3; 960], Evidence([0.3; 2]), f32::NAN),
        ] {
            let mut h = Harness::new();
            for _ in 0..4 {
                h.step(0.3);
            }
            assert!(
                !h.gate
                    .process(
                        pcm,
                        evidence,
                        h.start + Duration::from_millis(80),
                        false,
                        TransmitMode::VoiceActivity,
                        threshold,
                        true,
                        false
                    )
                    .active
            );
            for _ in 0..10 {
                assert!(!h.step(0.0).active);
            }
        }
    }

    // Labelled, generated waveforms; these are not recordings or listening scores.
    // Packet labels cover quiet onset/consonants/tail; pauses are not speech.
    fn fixture() -> Vec<([f32; 960], bool)> {
        let mut frames = Vec::new();
        let mut seed = 42_u32;
        for index in 0..260 {
            let speech = (120..150).contains(&index) || (158..174).contains(&index);
            let amplitude = match index {
                120..=121 | 135..=140 | 170..=173 => 0.004,
                122..=149 | 158..=169 => 0.04,
                40..=69 => 0.010, // Steady fan below manual threshold.
                70..=109 => 0.003 + (index - 70) as f32 * 0.00025,
                _ => 0.0,
            };
            let mut pcm = std::array::from_fn(|sample| {
                let t = (index * 960 + sample) as f32 / 48_000.0;
                if speech {
                    amplitude * (t * 220.0 * std::f32::consts::TAU).sin()
                } else {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    amplitude * (seed as f32 / u32::MAX as f32 * 2.0 - 1.0)
                }
            });
            // Sparse clicks and short keyboard-like decaying bursts.
            if [5, 20, 35, 80, 100, 210, 235].contains(&index) {
                for (n, sample) in pcm[..120].iter_mut().enumerate() {
                    *sample += 0.5 * (-(n as f32) / 20.0).exp();
                }
            }
            frames.push((pcm, speech));
        }
        frames
    }

    #[test]
    fn labelled_synthetic_comparison_with_legacy_peak_gate() {
        let fixture = fixture();
        let mut gate = Gate::default();
        let start = Instant::now();
        let (mut hold, mut old_open) = (0_usize, false);
        let (mut old_false, mut new_false, mut old_missed, mut new_missed) = (0, 0, 0, 0);
        for (index, (pcm, speech)) in fixture.iter().enumerate() {
            let peak = pcm.iter().fold(0.0_f32, |p, v| p.max(v.abs()));
            if peak >= THRESHOLD {
                hold = 10;
            } else {
                hold = hold.saturating_sub(1);
            }
            if hold > 0 && !old_open && !speech {
                old_false += 1;
            }
            old_open = hold > 0;
            if *speech && !old_open {
                old_missed += 1;
            }
            let was_open = gate.open;
            let evidence = Evidence(std::array::from_fn(|i| {
                Evidence::rms(pcm.as_chunks::<480>().0.get(i).unwrap())
            }));
            let out = gate.process(
                *pcm,
                evidence,
                start + Duration::from_millis(index as u64 * 20),
                false,
                TransmitMode::VoiceActivity,
                THRESHOLD,
                true,
                false,
            );
            if gate.open && !was_open && !speech {
                new_false += 1;
            }
            if index >= PRE_ROLL && fixture[index - PRE_ROLL].1 && !out.active {
                new_missed += 1;
            }
        }
        eprintln!(
            "labelled synthetic gate: false openings {old_false} -> {new_false}; missed speech packets {old_missed} -> {new_missed}; fixed added delay {} ms",
            PRE_ROLL * 20
        );
        assert_eq!((old_false, new_false), (7, 0));
        assert_eq!((old_missed, new_missed), (2, 0));
    }

    #[test]
    fn sustained_noise_above_sensitivity_can_open_energy_detector() {
        let mut h = Harness::new();
        for _ in 0..5 {
            h.step(0.02);
        }
        assert!(
            h.gate.open,
            "Do not claim speech classification or fan/TV rejection"
        );
    }
}
