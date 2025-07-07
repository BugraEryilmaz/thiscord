use ringbuf::{
    traits::{Consumer, Producer},
    HeapCons, HeapProd, HeapRb,
};
use shared::{Split, ROOM_SIZE};
use std::ops::Add;
use std::sync::{Arc, Mutex as StdMutex};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample, SupportedStreamConfig,
};

use crate::Error;

use super::AudioElement;

impl AudioElement {
    pub fn new() -> Self {
        let host = cpal::default_host();
        let mic = host.default_input_device();
        let speaker = host.default_output_device();
        AudioElement {
            speaker,
            mic,
            speaker_stream: None,
            mic_stream: None,
            mic_consumer: None,
            speaker_producers: None,
        }
    }

    pub fn get_config(&self) -> cpal::SupportedStreamConfig {
        SupportedStreamConfig::new(
            1,
            cpal::SampleRate(48000),
            cpal::SupportedBufferSize::Range { min: 960, max: 960 },
            cpal::SampleFormat::F32,
        )
    }

    pub fn start_speaker(&mut self) -> Result<Vec<Arc<StdMutex<HeapProd<f32>>>>, Error> {
        // If there is previously created speaker stream, stop it
        drop(self.speaker_stream.take());
        // Create ringbuffers for each person in the room
        let (tx_clone, (tx, rx)): (Vec<_>, (Vec<_>, Vec<_>)) = (0..ROOM_SIZE)
            .map(|_| {
                let (tx, rx) = HeapRb::<f32>::new(12000).split();
                let tx = Arc::new(StdMutex::new(tx));
                (tx.clone(), (tx, rx))
            })
            .unzip();
        self.speaker_producers = Some(tx_clone);
        if let Some(speaker) = self.speaker.as_ref() {
            // Start the output stream with the created ringbuffers
            let config = self.get_config();
            let stream = Self::make_speaker_stream(speaker, &config, rx)?;
            self.speaker_stream = Some(stream);
            // Start the stream
            self.speaker_stream.as_ref().unwrap().play()?;
        }

        Ok(tx)
    }

    pub fn make_speaker_stream(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        consumers: Vec<HeapCons<f32>>,
    ) -> Result<cpal::Stream, Error> {
        match config.sample_format() {
            cpal::SampleFormat::I16 => {
                Self::make_speaker_stream_from::<i16>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::F32 => {
                Self::make_speaker_stream_from::<f32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I8 => {
                Self::make_speaker_stream_from::<i8>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I32 => {
                Self::make_speaker_stream_from::<i32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I64 => {
                Self::make_speaker_stream_from::<i64>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U8 => {
                Self::make_speaker_stream_from::<u8>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U16 => {
                Self::make_speaker_stream_from::<u16>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U32 => {
                Self::make_speaker_stream_from::<u32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U64 => {
                Self::make_speaker_stream_from::<u64>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::F64 => {
                Self::make_speaker_stream_from::<f64>(device, &config.config(), consumers)
            }
            _ => todo!(),
        }
    }

    pub fn make_speaker_stream_from<T: SizedSample + FromSample<f32> + Add<Output = T>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut consumers: Vec<HeapCons<f32>>,
    ) -> Result<cpal::Stream, Error> {
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [T], _| {
                // Initialize output buffer with silence
                for d in data.iter_mut() {
                    *d = T::from_sample(0.0);
                }
                // Receive raw samples from encoder thread
                for sender in consumers.iter_mut() {
                    let sender: &mut HeapCons<f32> = sender;
                    let mut temp_data: Vec<f32> = vec![0.0; data.len()];
                    // Pop samples from the ring buffer
                    let cnt = sender.pop_slice(&mut temp_data);
                    if cnt == 0 {
                        // If no samples were received, fill with silence
                        continue;
                    }
                    // Convert f32 to T and write to output buffer
                    for (d, s) in data.iter_mut().zip(temp_data.iter()) {
                        *d = *d + T::from_sample(*s);
                    }
                }
            },
            |e| {
                eprintln!("Error: {}", e);
            },
            None,
        )?;
        Ok(stream)
    }

    pub fn start_mic(&mut self) -> Result<Arc<StdMutex<HeapCons<f32>>>, Error> {
        // If there is previously created mic stream, stop it
        drop(self.mic_stream.take());
        // Create a ringbuffer for microphone input
        let (mic_producer, mic_consumer) = HeapRb::<f32>::new(12000).split();
        let mic_consumer = Arc::new(StdMutex::new(mic_consumer));
        self.mic_consumer = Some(mic_consumer.clone());
        if let Some(mic) = self.mic.as_ref() {
            let config = self.get_config();
            // Start the input stream with the created ringbuffer
            let stream = Self::make_mic_stream(mic, &config, mic_producer)?;
            self.mic_stream = Some(stream);
            // Start the stream
            self.mic_stream.as_ref().unwrap().play()?;
        }
        Ok(mic_consumer)
    }

    pub fn make_mic_stream(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        tx: HeapProd<f32>,
    ) -> Result<cpal::Stream, Error> {
        match config.sample_format() {
            cpal::SampleFormat::I16 => {
                Self::make_mic_stream_from::<i16>(device, &config.config(), tx)
            }
            cpal::SampleFormat::F32 => {
                Self::make_mic_stream_from::<f32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::I8 => {
                Self::make_mic_stream_from::<i8>(device, &config.config(), tx)
            }
            cpal::SampleFormat::I32 => {
                Self::make_mic_stream_from::<i32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::I64 => {
                Self::make_mic_stream_from::<i64>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U8 => {
                Self::make_mic_stream_from::<u8>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U16 => {
                Self::make_mic_stream_from::<u16>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U32 => {
                Self::make_mic_stream_from::<u32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U64 => {
                Self::make_mic_stream_from::<u64>(device, &config.config(), tx)
            }
            cpal::SampleFormat::F64 => {
                Self::make_mic_stream_from::<f64>(device, &config.config(), tx)
            }
            _ => todo!(),
        }
    }

    pub fn make_mic_stream_from<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut tx: HeapProd<f32>,
    ) -> Result<cpal::Stream, Error>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let stream = device.build_input_stream(
            &config,
            move |data: &[T], _| {
                // Convert T to f32 and send to the encoder thread
                let samples = data.iter().map(|s| s.to_sample()).collect::<Vec<f32>>();
                // Note: This will block if the channel is full.
                tx.push_slice(&samples);
            },
            |e| {
                eprintln!("Error: {}", e);
            },
            None,
        )?;
        Ok(stream)
    }

    pub fn mute(&self) -> Result<(), Error> {
        if let Some(mic_stream) = self.mic_stream.as_ref() {
            mic_stream.pause()?;
        }
        Ok(())
    }

    pub fn unmute(&self) -> Result<(), Error> {
        if let Some(mic_stream) = self.mic_stream.as_ref() {
            mic_stream.play()?;
        }
        Ok(())
    }

    pub fn deafen(&self) -> Result<(), Error> {
        if let Some(speaker_stream) = self.speaker_stream.as_ref() {
            speaker_stream.pause()?;
        }
        Ok(())
    }

    pub fn undeafen(&self) -> Result<(), Error> {
        if let Some(speaker_stream) = self.speaker_stream.as_ref() {
            speaker_stream.play()?;
        }
        Ok(())
    }

    pub fn quit(&mut self) -> Result<(), Error> {
        if let Some(mic_stream) = self.mic_stream.take() {
            mic_stream.pause()?;
        }
        if let Some(speaker_stream) = self.speaker_stream.take() {
            speaker_stream.pause()?;
        }
        {
            self.mic_consumer.take();
            self.speaker_producers.take();
        }
        Ok(())
    }
}
