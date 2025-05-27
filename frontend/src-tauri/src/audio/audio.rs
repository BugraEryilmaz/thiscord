use ringbuf::{
    traits::{Consumer, Producer},
    HeapCons, HeapProd,
};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleRate, SupportedBufferSize, SupportedStreamConfig,
};

use crate::Error;

use super::{AudioCommand, AudioElement};

impl AudioElement {
    pub fn new(
        input_buffer: HeapProd<i16>,
        output_queue: Arc<Mutex<Vec<HeapCons<i16>>>>,
    ) -> Result<Self, Error> {
        Ok(AudioElement {
            input_queue: Arc::new(Mutex::new(input_buffer)),
            output_queue: output_queue,
            input_command_queue: Arc::new(Mutex::new(None)),
            output_command_queue: Arc::new(Mutex::new(None)),
        })
    }

    pub fn start_input_stream(&mut self) -> Result<(), Error> {
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(device) => device,
            None => return Err(Error::NoInputDevice),
        };

        let tx = self.input_queue.clone();
        let (command_tx, command_rx) = channel();
        drop(self.input_command_queue.lock().unwrap().replace(command_tx));
        std::thread::spawn(move || {
            // This thread will handle the audio input stream
            let mut current_stream = match Self::create_input_stream(device, tx.clone()) {
                Ok(stream) => stream,
                Err(e) => {
                    eprintln!("Failed to create input stream: {}", e);
                    return;
                }
            };
            match current_stream.play() {
                Ok(_) => println!("Input stream started successfully"),
                Err(e) => eprintln!("Failed to start input stream: {}", e),
            }
            // Handle commands from the main thread
            loop {
                match command_rx.recv() {
                    Ok(command) => {
                        match command {
                            // Handle commands like Stop, Pause, etc.
                            // For now, we just print the command
                            AudioCommand::Start => {
                                println!("Received Start command");
                                current_stream.play().unwrap_or_else(|e| {
                                    eprintln!("Failed to play input stream: {}", e);
                                });
                            }
                            AudioCommand::Stop => {
                                println!("Received Stop command");
                                current_stream.pause().unwrap_or_else(|e| {
                                    eprintln!("Failed to pause input stream: {}", e);
                                });
                            }
                            AudioCommand::AttachDevice(device) => {
                                current_stream = match Self::create_input_stream(device, tx.clone())
                                {
                                    Ok(stream) => stream,
                                    Err(e) => {
                                        eprintln!("Failed to create input stream: {}", e);
                                        continue;
                                    }
                                };
                                // Restart the stream with the new device
                                if let Err(e) = current_stream.play() {
                                    eprintln!("Failed to play new input stream: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error receiving command: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub fn create_input_stream(
        device: cpal::Device,
        tx: Arc<Mutex<HeapProd<i16>>>,
    ) -> Result<cpal::Stream, Error> {
        let supported_config = device.default_input_config()?;
        let sample_format = supported_config.sample_format();
        let config = supported_config.config();
        Ok(match sample_format {
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let mut tx = tx.lock().unwrap();
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
                    let mut tx = tx.lock().unwrap();
                    // Convert f32 to i16 and send
                    let samples: Vec<_> =
                        data.iter().map(|&f| (f * i16::MAX as f32) as i16).collect();
                    // Send the samples to the encoder thread
                    let _ = tx.push_slice(samples.as_slice());
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            _ => return Err(Error::NotImplemented),
        })
    }

    pub fn start_output_stream(&mut self) -> Result<(), Error> {
        let host = cpal::default_host();
        let device = match host.default_output_device() {
            Some(device) => device,
            None => return Err(Error::NoOutputDevice),
        };

        let rx = self.output_queue.clone();
        let (command_tx, command_rx) = channel();
        drop(
            self.output_command_queue
                .lock()
                .unwrap()
                .replace(command_tx),
        );

        std::thread::spawn(move || {
            // Handle commands from the main thread
            let mut current_stream = match Self::create_output_stream(device, rx.clone()) {
                Ok(stream) => stream,
                Err(e) => {
                    eprintln!("Failed to create output stream: {}", e);
                    return;
                }
            };
            match current_stream.play() {
                Ok(_) => println!("Output stream started successfully"),
                Err(e) => eprintln!("Failed to start output stream: {}", e),
            }
            loop {
                match command_rx.recv() {
                    Ok(command) => {
                        match command {
                            // Handle commands like Stop, Pause, etc.
                            // For now, we just print the command
                            AudioCommand::Start => {
                                println!("Received Start command");
                                current_stream.play().unwrap_or_else(|e| {
                                    eprintln!("Failed to play output stream: {}", e);
                                });
                            }
                            AudioCommand::Stop => {
                                println!("Received Stop command");
                                current_stream.pause().unwrap_or_else(|e| {
                                    eprintln!("Failed to pause output stream: {}", e);
                                });
                            }
                            AudioCommand::AttachDevice(device) => {
                                current_stream = Self::create_output_stream(device, rx.clone())
                                    .unwrap_or_else(|e| {
                                        eprintln!("Failed to create output stream: {}", e);
                                        return current_stream;
                                    });
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error receiving command: {}", e);
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    pub fn create_output_stream(
        device: cpal::Device,
        rx: Arc<Mutex<Vec<HeapCons<i16>>>>,
    ) -> Result<cpal::Stream, Error> {
        let supported_config = SupportedStreamConfig::new(
            1u16,
            SampleRate(48000),
            SupportedBufferSize::Range { min: 960, max: 960 },
            cpal::SampleFormat::F32,
        );
        let sample_format = supported_config.sample_format();
        let config = supported_config.config();
        Ok(match sample_format {
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    let mut rx = rx.lock().unwrap();
                    // Receive raw samples from encoder thread
                    for sender in rx.iter_mut() {
                        let mut temp_data: Vec<i16> = vec![0; data.len()];
                        // Pop samples from the ring buffer
                        let cnt = sender.pop_slice(&mut temp_data);
                        // Copy the samples to the output buffer
                        for (idx, (d, s)) in data.iter_mut().zip(temp_data.iter()).enumerate() {
                            if idx == 0 {
                                println!("First sample: d = {}, s = {}", *d, *s);
                                *d = *s; // First sample is directly assigned
                            } else {
                                *d = *d + *s;
                            }
                        }
                    }
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let mut rx = rx.lock().unwrap();
                    // Receive raw samples from encoder thread
                    for (idx, sender) in rx.iter_mut().enumerate() {
                        let mut temp_data: Vec<i16> = vec![0; data.len()];
                        // Pop samples from the ring buffer
                        let cnt = sender.pop_slice(&mut temp_data);
                        // Convert i16 to f32 and write to output buffer
                        println!("Processing {} samples, {:?}", cnt, temp_data);
                        for (d, s) in data.iter_mut().zip(temp_data.iter()) {
                            if idx == 0 {
                                println!("First sample: d = {}, s = {}", *d, *s);
                                *d = *s as f32 / i16::MAX as f32;
                            } else {
                                *d = *d + (*s as f32 / i16::MAX as f32);
                            }
                        }
                    }
                    // Clamp the output to prevent overflow
                    for sample in data.iter_mut() {
                        *sample = sample.clamp(-1.0, 1.0);
                    }
                },
                |e| {
                    eprintln!("Error: {}", e);
                },
                None,
            )?,
            _ => return Err(Error::NotImplemented),
        })
    }

    pub fn mute(&self) -> Result<(), Error> {
        if let Some(tx) = self.input_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Stop)?;
        }
        Ok(())
    }

    pub fn unmute(&self) -> Result<(), Error> {
        if let Some(tx) = self.input_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Start)?;
        }
        Ok(())
    }

    pub fn deafen(&self) -> Result<(), Error> {
        if let Some(tx) = self.output_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Stop)?;
        }
        Ok(())
    }

    pub fn undeafen(&self) -> Result<(), Error> {
        if let Some(tx) = self.output_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Start)?;
        }
        Ok(())
    }
}
