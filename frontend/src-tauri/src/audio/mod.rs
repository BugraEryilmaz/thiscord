use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex as StdMutex};

mod audio;
pub mod tauri;

pub struct AudioElement {
    pub mic_command_queue: Arc<StdMutex<Option<Sender<AudioCommand>>>>,
    pub speaker_command_queue: Arc<StdMutex<Option<Sender<AudioCommand>>>>,
}

pub enum AudioCommand {
    Start,
    Stop,
    // AttachDevice(cpal::Device),
    Quit,
    // SetVolume(f32),
    // etc.
}
