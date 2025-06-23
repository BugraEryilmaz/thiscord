use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use shared::{models::Users, TrackLocalStaticRTP};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::Error;

pub mod web;
pub mod backend;

pub static ROOM_SIZE: usize = 10;

pub struct VoiceRooms {
    pub voice_rooms: DashMap<Uuid, VoiceRoom>,
}
pub static VOICE_ROOMS: OnceLock<VoiceRooms> = OnceLock::new();
impl VoiceRooms {
    pub fn get_or_init() -> &'static VoiceRooms {
        VOICE_ROOMS.get_or_init(|| {
            let rooms = VoiceRooms {
                voice_rooms: DashMap::new(),
            };
            rooms
        })
    }
    pub fn get_room_or_init(&self, id: Uuid) -> VoiceRoom {
        self.voice_rooms
            .entry(id)
            .or_insert_with(|| VoiceRoom::new(id))
            .value()
            .clone()
    }
}

#[derive(Clone)]
pub struct VoiceRoom {
    pub id: Uuid,
    pub people: Arc<Mutex<[MaybeVoicePerson; ROOM_SIZE]>>,
}

impl VoiceRoom {
    pub fn new(id: Uuid) -> Self {
        VoiceRoom {
            id,
            people: Arc::new(Mutex::new(
                std::array::from_fn(|_| MaybeVoicePerson::new()),
            )),
        }
    }

    pub async fn join_person(
        &self,
        user: &Users,
        recv_tracks: Vec<Arc<TrackLocalStaticRTP>>,
    ) -> Result<usize, Error> {
        let mut people = self.people.lock().await;
        for (i, slot) in people.iter_mut().enumerate() {
            if slot.id.is_none() {
                slot.set_person(user, recv_tracks).await;
                return Ok(i);
            }
        }
        Err(Error::RoomFull)
    }

    pub async fn leave_person(&self, person_id: Uuid) -> Result<(), Error> {
        let mut people = self.people.lock().await;
        for slot in people.iter_mut() {
            if slot.id == Some(person_id) {
                slot.reset_person().await;
                return Ok(());
            }
        }
        Err(Error::UserNotFoundInRoom)
    }

    pub async fn get_track_i_of_all(
        &self,
        track_i: usize,
    ) -> Vec<Arc<Mutex<Option<Arc<TrackLocalStaticRTP>>>>> {
        let people = self.people.lock().await;
        people
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != track_i)
            .map(|(_, person)| person.recv_tracks[track_i].clone())
            .collect::<Vec<_>>()
    }
}

#[derive(Default)]
pub struct MaybeVoicePerson {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub recv_tracks: [Arc<Mutex<Option<Arc<TrackLocalStaticRTP>>>>; ROOM_SIZE],
}

impl MaybeVoicePerson {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_person(&mut self, user: &Users, recv_tracks: Vec<Arc<TrackLocalStaticRTP>>) {
        self.id = Some(user.id);
        self.name = Some(user.username.clone());
        for (track, recv_track) in self.recv_tracks.iter_mut().zip(recv_tracks) {
            *track.lock().await = Some(recv_track);
        }
    }

    pub async fn reset_person(&mut self) {
        self.id = None;
        self.name = None;
        for track in self.recv_tracks.iter_mut() {
            *track.lock().await = None;
        }
    }
}