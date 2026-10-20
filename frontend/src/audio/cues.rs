//! Server-selected notification earcons. Generate PCM before opening the device, then only
//! read samples in the callback. A bounded bitset coalesces bursts of changes.
use super::mixer::RATE;
use std::sync::OnceLock;

// Immutable PCM is shared between calls/device rebuilds. Initialization happens
// in Player::new on the control worker, never in the playback callback.
static SOUNDS: OnceLock<[Vec<f32>; 6]> = OnceLock::new();

#[cfg(test)]
pub(super) const JOIN: u8 = 1;
#[cfg(test)]
pub(super) const LEAVE: u8 = 2;

pub(super) struct Player {
    sounds: &'static [Vec<f32>; 6],
    pub volumes: [u8; 6],
    pending: u8,
    active: Option<usize>,
    position: usize,
}

impl Player {
    pub fn new() -> Self {
        Self {
            sounds: SOUNDS.get_or_init(|| {
                crate::notification_sound::MIXER_KINDS
                    .map(|kind| crate::notification_sound::render(kind, RATE))
            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_bounded_distinct_and_deafen_cancels_backlog() {
        let mut player = Player::new();
        assert_ne!(player.sounds[0], player.sounds[1]);
        for sound in player.sounds {
            assert!(sound.len() < RATE as usize * 2);
            assert!(sound.iter().all(|s| s.is_finite() && s.abs() <= 0.12));
            assert_eq!(sound[0], 0.0);
            assert_eq!(*sound.last().unwrap(), 0.0);
        }
        for _ in 0..1000 {
            player.request(JOIN | LEAVE, false);
        }
        let samples: Vec<_> = (0..RATE).map(|_| player.next()).collect();
        assert!(samples.iter().any(|s| s.abs() > 0.05));
        assert!(samples[RATE as usize * 3 / 4..].iter().all(|s| *s == 0.0));
        player.request(JOIN, false);
        player.next();
        player.request(LEAVE, true);
        player.request(0, false);
        assert!((0..RATE).all(|_| player.next() == 0.0));
    }

    #[test]
    fn shared_cues_keep_playback_positions_volumes_and_deafen_independent() {
        let mut first = Player::new();
        let mut second = Player::new();
        assert!(std::ptr::eq(first.sounds, second.sounds));
        first.volumes[0] = 50;
        first.request(JOIN, false);
        second.request(JOIN, false);
        for _ in 0..1000 {
            assert!((first.next() - second.next() * 0.5).abs() < 1e-7);
        }
        first.request(0, true);
        assert_eq!(first.next(), 0.0);
        assert!((0..1000).any(|_| second.next() != 0.0));
        let mut fresh = Player::new();
        fresh.request(JOIN, false);
        let sounds = fresh.sounds;
        for expected in &sounds[0][..1000] {
            assert!((fresh.next() - *expected).abs() < 1e-7);
        }
    }
}
