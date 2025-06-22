use my_web_rtc::Split;
use ringbuf::{
    traits::{Consumer, Producer},
    HeapCons, HeapProd, HeapRb,
};
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc::channel};
use std::sync::{Arc, Mutex as StdMutex};

use cpal::{
    traits::{DeviceTrait, StreamTrait},
    SampleRate, SupportedBufferSize, SupportedStreamConfig,
};

use crate::Error;

use super::{AudioCommand, AudioElement};

impl AudioElement {
    pub fn new() -> Self {
        AudioElement {
            mic_command_queue: Arc::new(StdMutex::new(None)),
            speaker_command_queue: Arc::new(StdMutex::new(None)),
        }
    }

    pub fn start_input_stream(&self, device: cpal::Device, dropped: Arc<AtomicBool>) -> Result<HeapCons<i16>, Error> {
        let (mic_producer, mic_consumer) = HeapRb::<i16>::new(12000).split();
        let (command_tx, command_rx) = channel();
        {
            self.mic_command_queue.lock().unwrap().replace(command_tx);
        }
        std::thread::spawn(move || {
            // This thread will handle the audio input stream
            let current_stream = match Self::create_input_stream(device, mic_producer) {
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
                            AudioCommand::Quit => {
                                println!("Received Quit command");
                                break; // Exit the loop and stop the thread
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error receiving command: {}", e);
                        break;
                    }
                }
            }
            dropped.store(true, Ordering::Relaxed);
        });

        Ok(mic_consumer)
    }

    pub fn create_input_stream(
        device: cpal::Device,
        mut tx: HeapProd<i16>,
    ) -> Result<cpal::Stream, Error> {
        let supported_config = device.default_input_config()?;
        let sample_format = supported_config.sample_format();
        let config = supported_config.config();
        Ok(match sample_format {
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

    pub fn start_output_stream(
        &self,
        device: cpal::Device,
        speaker_consumers: Vec<HeapCons<i16>>,
    ) -> Result<(), Error> {
        let (command_tx, command_rx) = channel();

        {
            self.speaker_command_queue
                .lock()
                .unwrap()
                .replace(command_tx);
        }

        std::thread::spawn(move || {
            // Handle commands from the main thread
            let current_stream = match Self::create_output_stream(device, speaker_consumers) {
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
                            AudioCommand::Quit => {
                                println!("Received Quit command");
                                break; // Exit the loop and stop the thread
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
        mut rx: Vec<HeapCons<i16>>,
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
                    // Initialize output buffer with silence
                    for d in data.iter_mut() {
                        *d = 0;
                    }
                    // Receive raw samples from encoder thread
                    for sender in rx.iter_mut() {
                        let mut temp_data: Vec<i16> = vec![0; data.len()];
                        // Pop samples from the ring buffer
                        let cnt = sender.pop_slice(&mut temp_data);
                        if cnt == 0 {
                            // If no samples were received, fill with silence
                            continue;
                        }
                        // Copy the samples to the output buffer
                        for (d, s) in data.iter_mut().zip(temp_data.iter()) {
                            *d = *d + *s;
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
                    // Initialize output buffer with silence
                    for d in data.iter_mut() {
                        *d = 0.0;
                    }
                    // Receive raw samples from encoder thread
                    for sender in rx.iter_mut() {
                        let mut temp_data: Vec<i16> = vec![0; data.len()];
                        // Pop samples from the ring buffer
                        let cnt = sender.pop_slice(&mut temp_data);
                        if cnt == 0 {
                            // If no samples were received, fill with silence
                            continue;
                        }
                        // Convert i16 to f32 and write to output buffer
                        for (d, s) in data.iter_mut().zip(temp_data.iter()) {
                            *d = *d + (*s as f32 / i16::MAX as f32);
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
        if let Some(tx) = self.mic_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Stop)?;
        }
        Ok(())
    }

    pub fn unmute(&self) -> Result<(), Error> {
        if let Some(tx) = self.mic_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Start)?;
        }
        Ok(())
    }

    pub fn deafen(&self) -> Result<(), Error> {
        if let Some(tx) = self.speaker_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Stop)?;
        }
        Ok(())
    }

    pub fn undeafen(&self) -> Result<(), Error> {
        if let Some(tx) = self.speaker_command_queue.lock().unwrap().as_mut() {
            tx.send(AudioCommand::Start)?;
        }
        Ok(())
    }

    pub fn quit(&self) -> Result<(), Error> {
        let mut speaker_command_queue = self.speaker_command_queue.lock().unwrap();
        if let Some(tx) = speaker_command_queue.as_mut() {
            tx.send(AudioCommand::Quit)?;
            *speaker_command_queue = None; 
        }
        let mut mic_command_queue = self.mic_command_queue.lock().unwrap();
        if let Some(tx) = mic_command_queue.as_mut() {
            tx.send(AudioCommand::Quit)?;
            *mic_command_queue = None;
        }
        Ok(())
    }
}
