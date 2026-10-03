use crate::{AccountId, ApiError, ChannelId, GuildId};
use serde::{Deserialize, Serialize};
pub const VOICE_PATH: &str = "/api/v1/voice";
pub const VOICE_VERSION: u8 = 1;
pub const ROOM_CAPACITY: usize = 8;
pub const SSRC_BASE: u32 = 10_000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Join {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Answer {
        sdp: String,
    },
    State {
        muted: bool,
        deafened: bool,
    },
    Screen {
        active: bool,
        audio: bool,
    },
    Ping {},
    Leave {},
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub account_id: AccountId,
    pub username: String,
    pub slot: usize,
    pub muted: bool,
    pub deafened: bool,
    pub can_speak: bool,
    #[serde(default)]
    pub sharing_screen: bool,
    #[serde(default)]
    pub sharing_audio: bool,
    #[serde(default)]
    pub screen_epoch: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Offer {
        sdp: String,
        slot: usize,
        can_speak: bool,
        #[serde(default)]
        screen_video: bool,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
    },
    Participants {
        members: Vec<Participant>,
    },
    Pong {},
    Revoked {},
    Error {
        error: ApiError,
    },
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceStatus {
    pub connected: bool,
    pub channel_id: Option<ChannelId>,
    pub message: String,
    pub participants: Vec<Participant>,
}
// TURN credentials are short-lived secrets; deliberately no Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

/// Media ordering is stable on the wire and in negotiated track slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MediaKind {
    Microphone = 0,
    ScreenVideo = 1,
    SystemAudio = 2,
}
impl MediaKind {
    pub const ALL: [Self; 3] = [Self::Microphone, Self::ScreenVideo, Self::SystemAudio];
    pub const TRACK_COUNT: usize = ROOM_CAPACITY * Self::ALL.len();
    pub const fn publisher_ssrc(self) -> u32 {
        900 + self as u32
    }
    pub const fn payload_type(self) -> u8 {
        if matches!(self, Self::ScreenVideo) {
            125
        } else {
            111
        }
    }
    pub const fn clock_rate(self) -> u32 {
        if matches!(self, Self::ScreenVideo) {
            90_000
        } else {
            48_000
        }
    }
    pub const fn track_index(self, slot: usize) -> usize {
        self as usize * ROOM_CAPACITY + slot
    }
    pub fn from_track(index: usize) -> Option<(Self, usize)> {
        Some((
            *Self::ALL.get(index / ROOM_CAPACITY)?,
            index % ROOM_CAPACITY,
        ))
    }
    pub const fn ssrc_base(self) -> u32 {
        match self {
            Self::Microphone => SSRC_BASE,
            Self::ScreenVideo => 20_000,
            Self::SystemAudio => 30_000,
        }
    }
    pub const fn relay_ssrc(self, slot: usize) -> u32 {
        self.ssrc_base() + slot as u32
    }
    pub fn from_relay_ssrc(ssrc: u32) -> Option<(Self, usize)> {
        Self::ALL.into_iter().find_map(|kind| {
            ssrc.checked_sub(kind.ssrc_base())
                .filter(|slot| (*slot as usize) < ROOM_CAPACITY)
                .map(|slot| (kind, slot as usize))
        })
    }
    pub fn from_publisher_ssrc(ssrc: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.publisher_ssrc() == ssrc)
    }
    pub fn mixer_slot(self, slot: usize) -> Option<usize> {
        match self {
            Self::Microphone => Some(slot),
            Self::SystemAudio => Some(ROOM_CAPACITY + slot),
            Self::ScreenVideo => None,
        }
    }
}
#[cfg(test)]
mod media_tests {
    use super::*;
    #[test]
    fn media_mappings_round_trip_and_reject_unknown_sources() {
        assert_eq!(
            MediaKind::ALL.map(MediaKind::publisher_ssrc),
            [900, 901, 902]
        );
        assert_eq!(
            MediaKind::ALL.map(MediaKind::ssrc_base),
            [10_000, 20_000, 30_000]
        );
        assert_eq!(MediaKind::ALL.map(MediaKind::payload_type), [111, 125, 111]);
        for kind in MediaKind::ALL {
            for slot in 0..ROOM_CAPACITY {
                assert_eq!(
                    MediaKind::from_track(kind.track_index(slot)),
                    Some((kind, slot))
                );
                assert_eq!(
                    MediaKind::from_relay_ssrc(kind.relay_ssrc(slot)),
                    Some((kind, slot))
                );
                assert_eq!(
                    MediaKind::from_publisher_ssrc(kind.publisher_ssrc()),
                    Some(kind)
                );
            }
        }
        assert_eq!(MediaKind::from_track(MediaKind::TRACK_COUNT), None);
        assert_eq!(MediaKind::from_relay_ssrc(20_008), None);
        assert_eq!(MediaKind::from_publisher_ssrc(903), None);
    }
}
