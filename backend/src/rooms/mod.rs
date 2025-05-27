pub mod web;

// static connection list
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use my_web_rtc::{TrackLocalStaticRTP, WebRTCConnection};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::Error;

pub struct Rooms {
    pub rooms: DashMap<Uuid, Room>,
}

const ROOM_SIZE: usize = 10;

#[derive(Clone)]
pub struct Room {
    pub id: Uuid,
    pub connections: Arc<Mutex<[Option<Arc<WebRTCConnection>>; ROOM_SIZE]>>,
    pub audio_tracks: Arc<Mutex<[Vec<Arc<Mutex<Option<Arc<TrackLocalStaticRTP>>>>>; ROOM_SIZE]>>,
}

static ROOMS: OnceLock<Rooms> = OnceLock::new();

impl Rooms {
    pub fn get_or_init() -> &'static Rooms {
        ROOMS.get_or_init(|| {
            let rooms = Rooms {
                rooms: DashMap::new(),
            };
            rooms
        })
    }
}

impl Room {
    pub fn new(id: Uuid) -> Self {
        Room {
            id,
            connections: Arc::new(Mutex::new(std::array::from_fn(|_| None))),
            audio_tracks: Arc::new(Mutex::new(std::array::from_fn(|_| {
                vec![Arc::new(Mutex::new(None)); ROOM_SIZE]
            }))),
        }
    }

    pub async fn join_user(&self, connection: Arc<WebRTCConnection>) -> Result<usize, Error> {
        let mut connections = self.connections.lock().await;
        for (idx, slot) in connections.iter_mut().enumerate() {
            if slot.is_none() {
                // Add 10 audio tracks for the new connection
                let audio_tracks = self.audio_tracks.lock().await;
                let new_tracks = connection.create_audio_track_rtp(ROOM_SIZE).await?;
                for (old, new) in audio_tracks[idx].iter().zip(new_tracks.iter()) {
                    *old.lock().await = Some(new.clone());
                }
                // Add the background receive and stream tasks
                let audio_tracks = audio_tracks
                    .iter()
                    .enumerate()
                    .filter(|(track_idx, _)| *track_idx != idx)
                    .map(|(_, track)| track[idx].clone())
                    .collect::<Vec<_>>();
                let receiver_queue = Arc::new(Mutex::new(None));
                connection
                    .background_stream_data(receiver_queue.clone(), audio_tracks)
                    .await?;
                connection.background_receive_data(receiver_queue).await?;
                *slot = Some(connection);
                return Ok(idx);
            }
        }
        Err(Error::RoomFull)
    }
}
