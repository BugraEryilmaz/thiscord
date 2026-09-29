//! Fixed-capacity SPSC queues. The device callback never allocates, locks or waits.
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

pub const RATE: u32 = 48_000;
pub const FRAME: usize = 960;
pub const MAX_STREAMS: usize = 32;
pub const CAPACITY: usize = FRAME * 6;

pub struct Controls {
    pub mute: AtomicBool,
    pub deafen: AtomicBool,
    pub push_to_talk: AtomicBool,
    pub pressed: AtomicBool,
    pub threshold: AtomicU32,
    pub master: AtomicU32,
    pub peak: AtomicU32,
    pub transmitting: AtomicBool,
    pub dropped: AtomicU64,
    pub underruns: AtomicU64,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            mute: false.into(),
            deafen: false.into(),
            push_to_talk: false.into(),
            pressed: false.into(),
            threshold: AtomicU32::new(0.015_f32.to_bits()),
            master: AtomicU32::new(1.0_f32.to_bits()),
            peak: AtomicU32::new(0),
            transmitting: false.into(),
            dropped: 0.into(),
            underruns: 0.into(),
        }
    }
}
pub struct StreamControl {
    pub volume: AtomicU32,
    pub active: AtomicBool,
    pub generation: AtomicU64,
}
pub struct StreamWriter {
    pub queue: HeapProd<f32>,
    pub control: Arc<StreamControl>,
}
struct StreamReader {
    queue: HeapCons<f32>,
    control: Arc<StreamControl>,
    generation: u64,
}
pub struct Mixer {
    streams: Vec<StreamReader>,
    controls: Arc<Controls>,
    reference: Option<HeapProd<f32>>,
}

pub fn mixer(controls: Arc<Controls>) -> (Vec<StreamWriter>, Mixer) {
    let mut writers = Vec::with_capacity(MAX_STREAMS);
    let mut readers = Vec::with_capacity(MAX_STREAMS);
    for _ in 0..MAX_STREAMS {
        let (queue, reader) = HeapRb::<f32>::new(CAPACITY).split();
        let control = Arc::new(StreamControl {
            volume: AtomicU32::new(1.0_f32.to_bits()),
            active: false.into(),
            generation: 0.into(),
        });
        writers.push(StreamWriter {
            queue,
            control: control.clone(),
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
            streams: readers,
            controls,
            reference: None,
        },
    )
}
impl StreamWriter {
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
    pub fn with_reference(mut self, reference: HeapProd<f32>) -> Self {
        self.reference = Some(reference);
        self
    }
    pub fn render<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        output: &mut [T],
        channels: usize,
    ) {
        let deafened = self.controls.deafen.load(Ordering::Relaxed);
        let master = f32::from_bits(self.controls.master.load(Ordering::Relaxed));
        // Catch up after a stall; never play a growing backlog of old speech.
        for stream in &mut self.streams {
            let generation = stream.control.generation.load(Ordering::Acquire);
            let changed = generation != stream.generation;
            stream.generation = generation;
            let discard = if changed || deafened || !stream.control.active.load(Ordering::Acquire) {
                stream.queue.occupied_len()
            } else {
                stream.queue.occupied_len().saturating_sub(FRAME * 3)
            };
            for _ in 0..discard {
                let _ = stream.queue.try_pop();
            }
        }
        let mut underruns = 0;
        for frame in output.chunks_mut(channels) {
            let mut mix = 0.0;
            for stream in &mut self.streams {
                if !stream.control.active.load(Ordering::Relaxed) {
                    continue;
                }
                match stream.queue.try_pop() {
                    Some(sample) if sample.is_finite() => {
                        mix +=
                            sample * f32::from_bits(stream.control.volume.load(Ordering::Relaxed))
                    }
                    Some(_) => {}
                    None => underruns += 1,
                }
            }
            let sample = if deafened {
                0.0
            } else {
                (mix * master).clamp(-1.0, 1.0)
            };
            frame.fill(T::from_sample(sample));
            if let Some(reference) = &mut self.reference {
                let _ = reference.try_push(sample);
            }
        }
        self.controls
            .underruns
            .fetch_add(underruns, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
