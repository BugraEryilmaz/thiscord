//! Server-selected notification earcons. Generate PCM before opening the device, then only
//! read samples in the callback. A bounded bitset coalesces bursts of changes.
use super::mixer::RATE;

#[cfg(test)]
pub(super) const JOIN: u8 = 1;
#[cfg(test)]
pub(super) const LEAVE: u8 = 2;

pub(super) struct Player {
    sounds: [Vec<f32>; 6],
    pub volumes: [u8; 6],
    pending: u8,
    active: Option<usize>,
    position: usize,
}

impl Player {
    pub fn new() -> Self {
        Self {
            sounds: [
                tone([660.0, 880.0]),
                tone([660.0, 440.0]),
                tone([660.0, 780.0]),
                tone([600.0, 660.0]),
                tone([740.0, 980.0]),
                tone([520.0, 660.0]),
            ],
            volumes: [100; 6],
            pending: 0,
            active: None,
            position: 0,
        }
    }

    pub fn request(&mut self, cues: u8, deafened: bool) {
        if deafened {
            self.pending = 0;
            self.active = None;
        } else {
            self.pending |= cues & 63;
        }
    }

    pub fn next(&mut self) -> f32 {
        if self.active.is_none() {
            if self.pending == 0 {
                return 0.0;
            }
            let index = self.pending.trailing_zeros() as usize;
            let bit = 1 << index;
            self.pending &= !bit;
            self.active = Some(index);
            self.position = 0;
        }
        let sound = &self.sounds[self.active.unwrap()];
        let sample = sound[self.position] * f32::from(self.volumes[self.active.unwrap()]) / 100.0;
        self.position += 1;
        if self.position == sound.len() {
            self.active = None;
        }
        sample
    }
}

fn tone(notes: [f32; 2]) -> Vec<f32> {
    // Two soft, 90 ms notes separated by 15 ms; cosine envelope avoids clicks.
    let length = RATE as usize * 90 / 1000;
    let gap = RATE as usize * 15 / 1000;
    let mut samples = Vec::with_capacity(length * 2 + gap);
    for (index, frequency) in notes.into_iter().enumerate() {
        if index != 0 {
            samples.extend(std::iter::repeat_n(0.0, gap));
        }
        for n in 0..length {
            let phase = std::f32::consts::TAU * frequency * n as f32 / RATE as f32;
            let envelope =
                0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / (length - 1) as f32).cos();
            samples.push(0.12 * envelope * phase.sin());
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_bounded_distinct_and_deafen_cancels_backlog() {
        let mut player = Player::new();
        assert_ne!(player.sounds[0], player.sounds[1]);
        for sound in &player.sounds {
            assert!(sound.len() < RATE as usize / 4);
            assert!(sound.iter().all(|s| s.is_finite() && s.abs() <= 0.12));
            assert_eq!(sound[0], 0.0);
            assert_eq!(*sound.last().unwrap(), 0.0);
        }
        for _ in 0..1000 {
            player.request(JOIN | LEAVE, false);
        }
        let samples: Vec<_> = (0..RATE).map(|_| player.next()).collect();
        assert!(samples.iter().any(|s| s.abs() > 0.05));
        assert!(samples[RATE as usize / 2..].iter().all(|s| *s == 0.0));
        player.request(JOIN, false);
        player.next();
        player.request(LEAVE, true);
        player.request(0, false);
        assert!((0..RATE).all(|_| player.next() == 0.0));
    }
}
