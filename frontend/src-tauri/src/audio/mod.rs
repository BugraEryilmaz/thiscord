use ringbuf::{HeapCons, HeapProd};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex as StdMutex};

mod audio;
pub mod tauri;

pub struct AudioElement {
    pub mic_producer: Arc<StdMutex<HeapProd<i16>>>,
    pub speaker_consumers: Arc<StdMutex<Vec<HeapCons<i16>>>>,
    pub mic_command_queue: Arc<StdMutex<Option<Sender<AudioCommand>>>>,
    pub speaker_command_queue: Arc<StdMutex<Option<Sender<AudioCommand>>>>,
}

pub enum AudioCommand {
    Start,
    Stop,
    AttachDevice(cpal::Device),
    Quit,
    // SetVolume(f32),
    // etc.
}
