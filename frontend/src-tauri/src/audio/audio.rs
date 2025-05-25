use ringbuf::{traits::{Consumer, Producer}, HeapCons, HeapProd};
use std::sync::Arc;

use cpal::{traits::{DeviceTrait, HostTrait, StreamTrait}, SampleRate, SupportedBufferSize, SupportedStreamConfig};

use crate::Error;

use super::AudioElement;

impl AudioElement {
    pub fn new() -> Result<Self, Error> {
        let host = cpal::default_host();
        let input_device = host.default_input_device();
        let output_device = host.default_output_device();
        Ok(AudioElement {
            input_device: input_device.map(Arc::new),
            output_device: output_device.map(Arc::new),
            input_stream: None,
            output_stream: None,
        })
    }

    pub fn start_input_stream(&mut self, mut tx: HeapProd<i16>) -> Result<(), Error> {
        let input_config = match self.input_device {
            Some(ref device) => device.default_input_config()?,
            None => return Err(Error::NoInputDevice),
        };
        let device = self.input_device.as_ref().unwrap();
        let sample_format = input_config.sample_format();
        let config: cpal::StreamConfig = input_config.into();

        let stream = match sample_format {
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    // Clone and send raw samples to encoder thread
                    let samples: Vec<i16> = data.to_vec();
                    // Note: This will block if the channel is full.
                    tx.push_slice(&samples);
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    // Convert f32 to i16 and send
                    let samples: Vec<_>  =
                        data.iter().map(|&f| (f * i16::MAX as f32) as i16).collect();
                    // Send the samples to the encoder thread
                    let pushed = tx.push_slice(samples.as_slice());
                    println!("Pushed {} samples from mic", pushed);
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            _ => return Err(Error::NotImplemented),
        };
        stream.play()?;
        self.input_stream = Some(Arc::new(stream));
        Ok(())
    }

    pub fn start_output_stream(&mut self, mut rx: HeapCons<i16>) -> Result<(), Error> {
        let output_config = match self.output_device {
            Some(_) => {
                SupportedStreamConfig::new(
                    1u16, 
                    SampleRate(48000), 
                    SupportedBufferSize::Range { min: 960, max: 960 }, 
                    cpal::SampleFormat::F32)
            }
            None => return Err(Error::NoInputDevice),
        };
        let device = self.output_device.as_ref().unwrap();
        let sample_format = output_config.sample_format();
        let config: cpal::StreamConfig = output_config.into();

        let stream = match sample_format {
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    // Receive raw samples from encoder thread
                    rx.pop_slice(data);
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    // Receive raw samples from encoder thread
                    let mut samples: Vec<i16> = vec![0; data.len()];
                    let read = rx.pop_slice(&mut samples);
                    println!("Read {} samples from audio stream to {} samples to output", read, data.len());
                    // Convert i16 to f32 and write to output buffer
                    let f32samples: Vec<f32> = samples.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    data.copy_from_slice(f32samples.as_slice());
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            _ => return Err(Error::NotImplemented),
        };
        stream.play()?;
        self.output_stream = Some(Arc::new(stream));
        Ok(())
    }
}
