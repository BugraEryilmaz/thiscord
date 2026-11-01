//! Soundboard clips: immutable PCM handed to the device callback through a
//! fixed-capacity SPSC queue. Finished clips return through a second queue so
//! their memory is always released on the control worker, never in the callback.
//! Each queued clip carries its own gain (the player's per-person soundboard level).
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::Arc;

pub type Clip = Arc<[i16]>;
/// Overlapping sounds mixed at once; later requests wait in the queue.
const ACTIVE: usize = 4;
const QUEUED: usize = 4;
/// Every clip in flight can be retired before the control worker drains.
const RETIRED: usize = ACTIVE + QUEUED;

/// Per-person soundboard levels are 0–200%; zero is never queued.
pub const MAX_GAIN: f32 = 2.0;

pub struct Sender {
    queue: HeapProd<(Clip, f32)>,
    retired: HeapCons<Clip>,
}

pub(super) struct Player {
    queue: HeapCons<(Clip, f32)>,
    retired: HeapProd<Clip>,
    active: [Option<(Clip, usize, f32)>; ACTIVE],
}

pub(super) fn channel() -> (Sender, Player) {
    let (queue, incoming) = HeapRb::<(Clip, f32)>::new(QUEUED).split();
    let (retired, returned) = HeapRb::<Clip>::new(RETIRED).split();
    (
        Sender {
            queue,
            retired: returned,
        },
        Player {
            queue: incoming,
            retired,
            active: Default::default(),
        },
    )
}

impl Sender {
    /// Control worker only. Returns false when the queue is full or nothing is audible.
    pub fn play(&mut self, clip: Clip, gain: f32) -> bool {
        while self.retired.try_pop().is_some() {}
        gain.is_finite()
            && gain > 0.0
            && gain <= MAX_GAIN
            && !clip.is_empty()
            && self.queue.try_push((clip, gain)).is_ok()
    }
}

impl Player {
    /// Once per callback: admit queued clips, or retire everything while deafened.
    pub fn begin(&mut self, deafened: bool) {
        for slot in &mut self.active {
            let finished = slot
                .as_ref()
                .is_some_and(|(clip, position, _)| deafened || *position >= clip.len());
            if finished && let Some((clip, _, _)) = slot.take() {
                // RETIRED covers every clip in flight; on overflow keep it parked.
                if let Err(clip) = self.retired.try_push(clip) {
                    let end = clip.len();
                    *slot = Some((clip, end, 0.0));
                }
            }
        }
        for slot in &mut self.active {
            if slot.is_some() {
                continue;
            }
            let Some((clip, gain)) = self.queue.try_pop() else {
                break;
            };
            if deafened {
                if let Err(clip) = self.retired.try_push(clip) {
                    let end = clip.len();
                    *slot = Some((clip, end, 0.0));
                }
            } else {
                *slot = Some((clip, 0, gain));
            }
        }
    }

    pub fn next(&mut self) -> f32 {
        let mut mix = 0.0;
        for (clip, position, gain) in self.active.iter_mut().flatten() {
            if let Some(sample) = clip.get(*position) {
                mix += f32::from(*sample) / 32768.0 * *gain;
                *position += 1;
            }
        }
        mix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_overlap_finish_and_return_to_the_sender() {
        let (mut sender, mut player) = channel();
        let clip: Clip = vec![16_384; 3].into();
        assert!(sender.play(clip.clone(), 1.0));
        assert!(sender.play(clip.clone(), 1.0));
        player.begin(false);
        assert_eq!(Arc::strong_count(&clip), 3);
        let samples: Vec<f32> = (0..4).map(|_| player.next()).collect();
        assert_eq!(samples, [1.0, 1.0, 1.0, 0.0]);
        player.begin(false);
        // Retired clips are still referenced until the sender drains them.
        assert_eq!(Arc::strong_count(&clip), 3);
        assert!(sender.play(clip.clone(), 1.0));
        assert_eq!(Arc::strong_count(&clip), 2);
        assert!(!sender.play(Vec::new().into(), 1.0));
        for gain in [0.0, -1.0, MAX_GAIN + 0.1, f32::NAN] {
            assert!(!sender.play(clip.clone(), gain));
        }
    }

    #[test]
    fn queue_is_bounded_and_deafen_discards_without_dropping_in_callback() {
        let (mut sender, mut player) = channel();
        let clip: Clip = vec![1_000; 100].into();
        for _ in 0..QUEUED {
            assert!(sender.play(clip.clone(), 1.0));
        }
        assert!(!sender.play(clip.clone(), 1.0));
        player.begin(false);
        assert!(player.next() > 0.0);
        assert!(sender.play(clip.clone(), 1.0));
        player.begin(true);
        assert_eq!(player.next(), 0.0);
        // Five clips went through the player; none was dropped in the callback.
        assert_eq!(Arc::strong_count(&clip), 1 + QUEUED + 1);
        assert!(sender.play(clip.clone(), 1.0));
        assert_eq!(Arc::strong_count(&clip), 2);
    }

    #[test]
    fn each_clip_keeps_its_own_gain_while_overlapping() {
        let (mut sender, mut player) = channel();
        let clip: Clip = vec![8_192; 2].into();
        assert!(sender.play(clip.clone(), 0.5));
        assert!(sender.play(clip.clone(), 2.0));
        player.begin(false);
        // 0.25 * 0.5 + 0.25 * 2.0
        assert_eq!(player.next(), 0.625);
    }
}
