use std::sync::{Arc, Mutex as StdMutex};

use ringbuf::HeapProd;
use ringbuf::HeapCons;
use shared::models::Channel;

use crate::models::LastUsedAudioDevices;
use crate::models::PerUserBoost;

mod audio;

pub struct AudioElement {
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
}

pub struct ChannelWithBoosts {
    pub channel: Channel,
    pub users: Vec<Option<PerUserBoost>>,
}