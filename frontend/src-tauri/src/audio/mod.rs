use std::sync::{Arc, Mutex as StdMutex};

use front_shared::models::audio_config::AudioConfigDBPartial;
use ringbuf::HeapProd;
use ringbuf::HeapCons;
use shared::models::Channel;
use shared::ROOM_SIZE;
use webrtc_audio_processing::Processor;

use front_shared::models::audio_config::AudioConfig;
use front_shared::models::last_used_devices::LastUsedAudioDevices;
use front_shared::models::user_boost::PerUserBoost;

mod audio;

pub struct AudioElement {
    pub audio_processor: Processor,
    pub audio_processor_config: AudioConfig,
    pub channel_with_boosts: Option<ChannelWithBoosts>,
    pub devices: LastUsedAudioDevices,
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
    SetMicBoost(i32),
    SetSpeakerBoost(i32),
    SetUserBoost {
        user_id: uuid::Uuid,
        boost_level: i32,
    },
    ChangeSetting {
        cfg: AudioConfigDBPartial
    }
}

pub struct ChannelWithBoosts {
    pub channel: Channel,
    pub users: [PerUserBoost; ROOM_SIZE],
}