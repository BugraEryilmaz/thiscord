use std::sync::Arc;

use cpal::Stream;

mod audio;

pub struct AudioElement {
    pub input_device: Option<Arc<cpal::Device>>,
    pub output_device: Option<Arc<cpal::Device>>,
    pub input_stream: Option<Arc<Stream>>,
    pub output_stream: Option<Arc<Stream>>,
}