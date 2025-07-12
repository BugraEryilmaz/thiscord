use std::sync::{Arc, Mutex as StdMutex};

use ringbuf::HeapProd;
use ringbuf::HeapCons;

mod audio;

pub struct AudioElement {
    pub speaker: Option<cpal::Device>,
    pub mic: Option<cpal::Device>,
    pub speaker_stream: Option<cpal::Stream>,
    pub mic_stream: Option<cpal::Stream>,
    pub mic_consumer: Option<Arc<StdMutex<HeapCons<f32>>>>,
    pub speaker_producers: Option<Vec<Arc<StdMutex<HeapProd<f32>>>>>,
}

pub enum AudioCommand {
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    Quit,
    SetMic(String),
    SetSpeaker(String),
}
