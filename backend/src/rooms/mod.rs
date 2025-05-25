pub mod web;

// static connection list
use std::{sync::{Arc, OnceLock}};

use uuid::Uuid;
use dashmap::DashMap;
use my_web_rtc::WebRTCConnection;

static ROOMS: OnceLock<DashMap<Uuid, Arc<WebRTCConnection>>> = OnceLock::new();

pub fn get_rooms() -> &'static DashMap<Uuid, Arc<WebRTCConnection>> {
    ROOMS.get_or_init(|| DashMap::new())
}