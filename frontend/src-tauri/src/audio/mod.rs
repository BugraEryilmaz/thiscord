use ringbuf::{HeapCons, HeapProd};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

mod audio;
pub mod tauri;

pub struct AudioElement {
    pub input_queue: Arc<Mutex<HeapProd<i16>>>,
    pub output_queue: Arc<Mutex<Vec<HeapCons<i16>>>>,
    pub input_command_queue: Arc<Mutex<Option<Sender<AudioCommand>>>>,
    pub output_command_queue: Arc<Mutex<Option<Sender<AudioCommand>>>>,
}

pub enum AudioCommand {
    Start,
    Stop,
    AttachDevice(cpal::Device),
    // SetVolume(f32),
    // etc.
}
