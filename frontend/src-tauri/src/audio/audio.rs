use ringbuf::{
    traits::{Consumer, Producer},
    HeapCons, HeapProd, HeapRb,
};
use shared::{
    models::{AudioChannelMemberUpdate, ChannelWithUsers},
    Split, ROOM_SIZE,
};
use std::sync::{atomic::AtomicI32, Arc, Mutex as StdMutex};
use std::{
    iter,
    ops::{Add, Div, Mul},
    sync::atomic::Ordering,
};
use tauri::{AppHandle, Manager};
use webrtc_audio_processing::{InitializationConfig, Processor};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample, SupportedStreamConfig,
};

use front_shared::models::last_used_devices::{LastUsedAudioDevices, LastUsedAudioDevicesWString};
use front_shared::models::audio_config::AudioConfig;
use front_shared::models::user_boost::PerUserBoost;

use crate::{
    audio::ChannelWithBoosts,
    utils::{establish_connection, AppState},
    Error,
};

use super::AudioElement;

impl AudioElement {
    pub fn new(handle: AppHandle) -> Self {
        let state = handle.state::<AppState>();
        let devices =
            LastUsedAudioDevices::get_from_db_or_default(&mut establish_connection(&handle))
                .unwrap_or_default();
        // Set the current mic and speaker in the app state
        {
            let mut last_used_audio_devices = state.last_used_audio_devices.lock().unwrap();
            *last_used_audio_devices = Some(devices.clone().into());
        }
        let initialization_config = InitializationConfig {
            num_capture_channels: 1,
            num_render_channels: 1,
            sample_rate_hz: 48_000,
        };
        // SAFETY: we have just created the processor with a valid config
        let mut audio_processor =
            Processor::new(&initialization_config).expect("Failed to create audio processor");
        let mut conn = establish_connection(&handle);
        let audio_processor_config = AudioConfig::get(&mut conn);
        audio_processor.set_config(audio_processor_config.cfg.clone());
        // Give an zero speaker stream to initialize
        let mut speaker_stream: [f32; 480] = [0.0; 480]; // 10 ms of silence at 48 kHz
        for _ in 0..15 {
            audio_processor
                .process_render_frame(&mut speaker_stream)
                .expect("Failed to process initial speaker stream");
        }
        audio_processor.initialize();
        AudioElement {
            audio_processor,
            audio_processor_config,
            channel_with_boosts: None,
            devices,
            speaker_stream: None,
            mic_stream: None,
            mic_consumer: None,
            speaker_producers: None,
        }
    }

    pub fn set_channel(&mut self, channel_with_users: &ChannelWithUsers, handle: AppHandle) {
        let mut user_boosts: [PerUserBoost; ROOM_SIZE] = Default::default();

        for user in &channel_with_users.users {
            user_boosts[user.slot] = PerUserBoost::get(&mut establish_connection(&handle), user.id);
        }
        self.channel_with_boosts = Some(ChannelWithBoosts {
            channel: channel_with_users.channel.clone(),
            users: user_boosts,
        });
    }

    pub fn clear_channel(&mut self) {
        self.channel_with_boosts = None;
    }

    pub fn handle_join_channel(
        &mut self,
        data: &AudioChannelMemberUpdate,
        handle: AppHandle,
    ) -> Result<(), Error> {
        if let Some(channel_with_boosts) = &mut self.channel_with_boosts {
            if channel_with_boosts.channel.id != data.channel.id {
                return Ok(());
            }
            let boost = PerUserBoost::get(&mut establish_connection(&handle), data.user.id);
            channel_with_boosts.users[data.user.slot].user_id = Some(data.user.id);
            channel_with_boosts.users[data.user.slot]
                .boost_level
                .store(boost.boost_level.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn handle_leave_channel(&mut self, data: &AudioChannelMemberUpdate) -> Result<(), Error> {
        if let Some(channel_with_boosts) = &mut self.channel_with_boosts {
            if channel_with_boosts.channel.id != data.channel.id {
                return Ok(());
            }
            channel_with_boosts.users[data.user.slot].user_id = None;
            channel_with_boosts.users[data.user.slot]
                .boost_level
                .store(100, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn set_user_boost(
        &mut self,
        user_id: uuid::Uuid,
        boost: i32,
        handle: AppHandle,
    ) -> Result<(), Error> {
        if let Some(channel_with_boosts) = &mut self.channel_with_boosts {
            for user in channel_with_boosts.users.iter() {
                if let Some(id) = user.user_id {
                    if id == user_id {
                        user.boost_level
                            .store(boost, std::sync::atomic::Ordering::Relaxed);
                        user.save(&mut establish_connection(&handle))?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn set_default_user_boost(
        user_id: uuid::Uuid,
        boost: i32,
        handle: AppHandle,
    ) -> Result<(), Error> {
        let conn = &mut establish_connection(&handle);
        let user_boost = PerUserBoost::get(conn, user_id);
        user_boost
            .boost_level
            .store(boost, std::sync::atomic::Ordering::Relaxed);
        user_boost.save(conn)?;
        Ok(())
    }

    pub fn get_config() -> cpal::SupportedStreamConfig {
        SupportedStreamConfig::new(
            1,
            cpal::SampleRate(48000),
            cpal::SupportedBufferSize::Range { min: 480, max: 480 },
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
        if let Some(speaker) = self.devices.speaker.as_ref() {
            // Start the output stream with the created ringbuffers
            let config = Self::get_config();
            let stream = self.make_speaker_stream(speaker, &config, rx)?;
            self.speaker_stream = Some(stream);
            // Start the stream
            self.speaker_stream.as_ref().unwrap().play()?;
        }

        Ok(tx)
    }

    pub fn start_mic(&mut self) -> Result<Arc<StdMutex<HeapCons<f32>>>, Error> {
        // If there is previously created mic stream, stop it
        drop(self.mic_stream.take());
        // Create a ringbuffer for microphone input
        let (mic_producer, mic_consumer) = HeapRb::<f32>::new(12000).split();
        let mic_consumer = Arc::new(StdMutex::new(mic_consumer));
        self.mic_consumer = Some(mic_consumer.clone());
        if let Some(mic) = self.devices.mic.as_ref() {
            let config = Self::get_config();
            // Start the input stream with the created ringbuffer
            let stream = self.make_mic_stream(mic, &config, mic_producer)?;
            self.mic_stream = Some(stream);
            // Start the stream
            self.mic_stream.as_ref().unwrap().play()?;
        }
        Ok(mic_consumer)
    }

    pub fn change_speaker(&mut self, device_name: &str, state: &AppState) -> Result<(), Error> {
        // Set the current speaker in the app state
        {
            let mut last_used_audio_devices = state.last_used_audio_devices.lock().unwrap();
            last_used_audio_devices
                .as_mut()
                .map(|devices| devices.speaker = Some(device_name.to_string()));
        }
        // If there is previously created speaker stream, stop it
        drop(self.speaker_stream.take());
        // Find the device by name
        let host = cpal::default_host();
        let device = host
            .output_devices()?
            .find(|d| d.name().unwrap_or_default() == device_name)
            .ok_or_else(|| Error::DeviceNotFound(device_name.to_string()))?;
        // Create ringbuffers for each person in the room
        let (tx, rx): (Vec<_>, Vec<_>) = (0..ROOM_SIZE)
            .map(|_| HeapRb::<f32>::new(12000).split())
            .unzip();
        if let Some(speaker_producers) = self.speaker_producers.as_ref() {
            for (speaker_producer, tx) in speaker_producers.iter().zip(tx.into_iter()) {
                *speaker_producer.lock().unwrap() = tx;
            }
        }
        // Start the output stream with the created ringbuffers
        let config = Self::get_config();
        let stream = self.make_speaker_stream(&device, &config, rx)?;
        self.devices.speaker = Some(device);
        self.speaker_stream = Some(stream);
        // Start the stream
        self.speaker_stream.as_ref().unwrap().play()?;
        Ok(())
    }

    pub fn change_speaker_boost(&mut self, boost: i32, state: &AppState) {
        {
            let mut last_used_audio_devices = state.last_used_audio_devices.lock().unwrap();
            if let Some(devices) = last_used_audio_devices.as_mut() {
                devices.speaker_boost = Some(boost);
            }
        }
        self.devices.speaker_boost = Some(boost);
        if let Some(speaker) = self.devices.speaker.as_ref() {
            let speaker_name = speaker.name().unwrap_or_default();
            if let Err(e) = self.change_speaker(&speaker_name, state) {
                tracing::error!("Failed to restart speaker with boost: {}", e);
            }
        }
    }

    pub fn set_default_speaker(device_name: &str, handle: AppHandle) {
        let conn = &mut establish_connection(&handle);
        let mut devices = LastUsedAudioDevicesWString::get_from_db(conn).unwrap_or_default();
        devices.speaker = Some(device_name.to_string());
        devices.save_to_db(conn).unwrap_or_else(|e| {
            tracing::error!("Failed to save default speaker: {}", e);
        });
    }

    pub fn set_default_speaker_boost(boost: i32, handle: AppHandle) {
        let conn = &mut establish_connection(&handle);
        let mut devices = LastUsedAudioDevicesWString::get_from_db(conn).unwrap_or_default();
        devices.speaker_boost = Some(boost);
        devices.save_to_db(conn).unwrap_or_else(|e| {
            tracing::error!("Failed to save default speaker boost: {}", e);
        });
    }

    pub fn change_mic(&mut self, device_name: &str, state: &AppState) -> Result<(), Error> {
        // Set the current mic in the app state
        {
            let mut last_used_audio_devices = state.last_used_audio_devices.lock().unwrap();
            last_used_audio_devices
                .as_mut()
                .map(|devices| devices.mic = Some(device_name.to_string()));
        }
        // If there is previously created mic stream, stop it
        drop(self.mic_stream.take());
        // Find the device by name
        let host = cpal::default_host();
        let device = host
            .input_devices()?
            .find(|d| d.name().unwrap_or_default() == device_name)
            .ok_or_else(|| Error::DeviceNotFound(device_name.to_string()))?;
        // Create a ringbuffer for microphone input
        let (mic_producer, mic_consumer) = HeapRb::<f32>::new(12000).split();
        {
            if let Some(mic_consumer_arc) = self.mic_consumer.as_ref() {
                *mic_consumer_arc.lock().unwrap() = mic_consumer;
            }
        }
        // Start the input stream with the created ringbuffer
        let config = Self::get_config();
        let stream = self.make_mic_stream(&device, &config, mic_producer)?;
        self.devices.mic = Some(device);
        self.mic_stream = Some(stream);
        // Start the stream
        self.mic_stream.as_ref().unwrap().play()?;
        Ok(())
    }

    pub fn change_mic_boost(&mut self, boost: i32, state: &AppState) {
        {
            let mut last_used_audio_devices = state.last_used_audio_devices.lock().unwrap();
            if let Some(devices) = last_used_audio_devices.as_mut() {
                devices.mic_boost = Some(boost);
            }
        }
        self.devices.mic_boost = Some(boost);
        if let Some(mic) = self.devices.mic.as_ref() {
            let mic_name = mic.name().unwrap_or_default();
            if let Err(e) = self.change_mic(&mic_name, state) {
                tracing::error!("Failed to restart mic with boost: {}", e);
            }
        }
    }

    pub fn set_default_mic(device_name: &str, handle: AppHandle) {
        let conn = &mut establish_connection(&handle);
        let mut devices = LastUsedAudioDevicesWString::get_from_db(conn).unwrap_or_default();
        devices.mic = Some(device_name.to_string());
        devices.save_to_db(conn).unwrap_or_else(|e| {
            tracing::error!("Failed to save default mic: {}", e);
        });
    }

    pub fn set_default_mic_boost(boost: i32, handle: AppHandle) {
        let conn = &mut establish_connection(&handle);
        let mut devices = LastUsedAudioDevicesWString::get_from_db(conn).unwrap_or_default();
        devices.mic_boost = Some(boost);
        devices.save_to_db(conn).unwrap_or_else(|e| {
            tracing::error!("Failed to save default mic boost: {}", e);
        });
    }

    pub fn list_speakers() -> Result<Vec<String>, Error> {
        let host = cpal::default_host();
        let devices = host.output_devices()?;
        let mut speakers = Vec::new();
        for device in devices {
            if let Ok(name) = device.name() {
                speakers.push(name);
            }
        }
        Ok(speakers)
    }

    pub fn list_mics() -> Result<Vec<String>, Error> {
        let host = cpal::default_host();
        let devices = host.input_devices()?;
        let mut mics = Vec::new();
        for device in devices {
            if let Ok(name) = device.name() {
                mics.push(name);
            }
        }
        Ok(mics)
    }

    pub fn make_speaker_stream(
        &self,
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        consumers: Vec<HeapCons<f32>>,
    ) -> Result<cpal::Stream, Error> {
        match config.sample_format() {
            cpal::SampleFormat::I16 => {
                self.make_speaker_stream_from::<i16>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::F32 => {
                self.make_speaker_stream_from::<f32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I8 => {
                self.make_speaker_stream_from::<i8>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I32 => {
                self.make_speaker_stream_from::<i32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::I64 => {
                self.make_speaker_stream_from::<i64>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U8 => {
                self.make_speaker_stream_from::<u8>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U16 => {
                self.make_speaker_stream_from::<u16>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U32 => {
                self.make_speaker_stream_from::<u32>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::U64 => {
                self.make_speaker_stream_from::<u64>(device, &config.config(), consumers)
            }
            cpal::SampleFormat::F64 => {
                self.make_speaker_stream_from::<f64>(device, &config.config(), consumers)
            }
            _ => todo!(),
        }
    }

    pub fn make_speaker_stream_from<
        T: SizedSample
            + FromSample<f32>
            + FromSample<i32>
            + Add<Output = T>
            + Mul<Output = T>
            + Div<Output = T>,
    >(
        &self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut consumers: Vec<HeapCons<f32>>,
    ) -> Result<cpal::Stream, Error> {
        let boost = self.devices.speaker_boost.unwrap_or(100);
        let mut processor = self.audio_processor.clone();
        let user_boosts = self
            .channel_with_boosts
            .as_ref()
            .map(|c| {
                c.users
                    .as_ref()
                    .iter()
                    .map(|b| b.boost_level.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                tracing::warn!(
                    "No channel with boosts set, using default boosts (NOT CHANGEABLE AFTERWARDS)"
                );
                iter::repeat(Arc::new(AtomicI32::new(100)))
                    .take(ROOM_SIZE)
                    .collect::<Vec<_>>()
            });
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [T], _| {
                let mut data_f32: Vec<f32> = vec![0.0; data.len()];
                // Receive raw samples from encoder thread
                for (idx, sender) in consumers.iter_mut().enumerate() {
                    let sender: &mut HeapCons<f32> = sender;
                    let mut temp_data: Vec<f32> = vec![0.0; data.len()];
                    // Pop samples from the ring buffer
                    let cnt = sender.pop_slice(&mut temp_data);
                    if cnt == 0 {
                        // If no samples were received, fill with silence
                        continue;
                    }
                    // Apply user boosts
                    let user_boost = user_boosts[idx].clone();
                    // Convert f32 to T and write to output buffer
                    for (d, s) in data_f32.iter_mut().zip(temp_data.iter()) {
                        *d = *d + *s * user_boost.load(Ordering::Relaxed) as f32 / 100.0;
                    }
                }
                // Apply the speaker boost
                for d in data_f32.iter_mut() {
                    *d = *d * boost as f32 / 100.0;
                }
                // Process the samples with the audio processor
                // SAFETY: we need to ensure that each frame is 480 samples long
                let mut data_f32_chunks = data_f32.chunks_exact_mut(480);
                for chunk in &mut data_f32_chunks {
                    if let Err(processed_samples) =
                        processor.process_render_frame(chunk)
                    {
                        tracing::error!("Error processing audio frame: {}", processed_samples);
                    }
                }
                if data_f32_chunks.into_remainder().len() > 0 {
                    tracing::warn!("Speaker stream received a frame with less than 480 samples, this is not supported by the audio processor");
                }
                // Convert f32 to T and write to output buffer
                for (d, s) in data.iter_mut().zip(data_f32.iter()) {
                    *d = T::from_sample(*s);
                }
            },
            |e| {
                tracing::error!("Error in speaker stream: {}", e);
            },
            None,
        )?;
        Ok(stream)
    }

    pub fn make_mic_stream(
        &self,
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        tx: HeapProd<f32>,
    ) -> Result<cpal::Stream, Error> {
        match config.sample_format() {
            cpal::SampleFormat::I16 => {
                self.make_mic_stream_from::<i16>(device, &config.config(), tx)
            }
            cpal::SampleFormat::F32 => {
                self.make_mic_stream_from::<f32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::I8 => self.make_mic_stream_from::<i8>(device, &config.config(), tx),
            cpal::SampleFormat::I32 => {
                self.make_mic_stream_from::<i32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::I64 => {
                self.make_mic_stream_from::<i64>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U8 => self.make_mic_stream_from::<u8>(device, &config.config(), tx),
            cpal::SampleFormat::U16 => {
                self.make_mic_stream_from::<u16>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U32 => {
                self.make_mic_stream_from::<u32>(device, &config.config(), tx)
            }
            cpal::SampleFormat::U64 => {
                self.make_mic_stream_from::<u64>(device, &config.config(), tx)
            }
            cpal::SampleFormat::F64 => {
                self.make_mic_stream_from::<f64>(device, &config.config(), tx)
            }
            _ => todo!(),
        }
    }

    pub fn make_mic_stream_from<T>(
        &self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut tx: HeapProd<f32>,
    ) -> Result<cpal::Stream, Error>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let boost = self.devices.mic_boost.unwrap_or(100);
        let mut processor = self.audio_processor.clone();
        let stream = device.build_input_stream(
            &config,
            move |data: &[T], _| {
                // Convert T to f32 and send to the encoder thread
                let mut samples = data
                    .iter()
                    .map(|s| s.to_sample::<f32>() * (boost as f32) / 100.0)
                    .collect::<Vec<f32>>();
                // Process the samples with the audio processor
                // SAFETY: we need to ensure that each frame is 480 samples long
                let mut samples_chunks = samples.chunks_exact_mut(480);
                for chunk in &mut samples_chunks {
                    if let Err(processed_samples) =
                        processor.process_capture_frame(chunk)
                    {
                        tracing::error!("Error processing audio frame: {}", processed_samples);
                    }
                }
                if samples_chunks.into_remainder().len() > 0 {
                    tracing::warn!("Mic stream received a frame with less than 480 samples, this is not supported by the audio processor");
                }
                // Note: This will block if the channel is full.
                tx.push_slice(&samples);
            },
            |e| {
                tracing::error!("Error in mic stream: {}", e);
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
