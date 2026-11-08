use crate::{AccountId, ApiError, ChannelId, GuildId};
use serde::{Deserialize, Serialize};
pub const VOICE_PATH: &str = "/api/v1/voice";
pub const VOICE_VERSION: u8 = 1;
pub const ROOM_CAPACITY: usize = 8;
pub const SSRC_BASE: u32 = 10_000;
/// A displaced connection must never automatically take the call back.
pub const REPLACED_CLOSE_CODE: u16 = 4001;
/// A lost signaling transport may be reattached without renegotiating media.
pub const SIGNALING_GRACE_SECS: u64 = 30;
/// WebSocket close code for a planned server restart (RFC 6455 "Service Restart").
/// Clients reconnect and rejoin after fresh admission; it never means revocation.
pub const RESTART_CLOSE_CODE: u16 = 1012;
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinAttempt {
    /// The first upgrade's request ID, retained across automatic retries.
    pub call_id: crate::RequestId,
    pub reconnect: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFrame {
    pub version: u8,
    pub event: ClientEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientEvent {
    Resume {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
        resume_token: String,
        sequence: u64,
    },
    Join {
        token: String,
        guild_id: GuildId,
        channel_id: ChannelId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<JoinAttempt>,
    },
    MicrophoneBoundary {
        generation: u64,
    },
    MicrophoneRefresh {
        generation: u64,
        challenge: u32,
    },
    MicrophoneRefreshReady {
        generation: u64,
        challenge: u32,
    },
    AudioReply {
        request_id: crate::RequestId,
        chunk: String,
        done: bool,
    },
    Answer {
        /// A browser WebRTC stack that cannot choose RTP SSRCs. Its single
        /// Opus send track is the microphone, whatever SSRC it uses.
        #[serde(default, skip_serializing_if = "is_false")]
        browser_publisher: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        packet_redundancy: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        remote_audio: bool,
        sdp: String,
        #[serde(default, skip_serializing_if = "is_false")]
        screen_feedback: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        screen_subscriptions: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        screen_layers: bool,
        /// Accepts the read-only `video_diagnostics` admin command.
        #[serde(default, skip_serializing_if = "is_false")]
        video_diagnostics: bool,
        /// Accepts `ServerEvent::Soundboard`; only sent when the offer advertised it.
        #[serde(default, skip_serializing_if = "is_false")]
        soundboard: bool,
        /// Sends per-view `h264_high` and accepts `ScreenTarget::profile`. Only
        /// sent when the offer advertised `screen_profiles`.
        #[serde(default, skip_serializing_if = "is_false")]
        screen_profiles: bool,
        /// Accepts `ServerEvent::Restarting`. Only sent when the offer advertised it.
        #[serde(default, skip_serializing_if = "is_false")]
        restart_notice: bool,
        /// May send `ClientEvent::IceRestart`. Only sent when the offer advertised it.
        #[serde(default, skip_serializing_if = "is_false")]
        ice_restart: bool,
    },
    /// Plays a guild sound for everyone in this call. Requires a negotiated soundboard.
    Soundboard {
        sound_id: crate::SoundId,
    },
    State {
        muted: bool,
        deafened: bool,
    },
    Camera {
        active: bool,
    },
    Screen {
        active: bool,
        audio: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<crate::screen::Quality>,
    },
    ScreenKeyframe {
        #[serde(default = "screen_kind", skip_serializing_if = "is_screen_kind")]
        kind: MediaKind,
        slot: usize,
        epoch: u32,
    },
    ScreenViews {
        views: Vec<crate::screen::Subscription>,
    },
    /// Asks for an ICE restart of the existing media session (path loss, network
    /// change or relay credentials nearing expiry). Only after `ice_restart` was
    /// negotiated; the server answers with `ServerEvent::IceRestart`.
    IceRestart {},
    /// The answer to `ServerEvent::IceRestart`, sent once local gathering ends.
    IceRestartAnswer {
        sdp: String,
    },
    Ping {},
    Leave {},
}
fn accepted_by_default() -> bool {
    true
}
pub(crate) fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// A synthetic voice participant with no account credentials or profile.
    #[serde(default, skip_serializing_if = "is_false")]
    pub test_bot: bool,
    pub account_id: AccountId,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_id: Option<crate::AvatarId>,
    pub slot: usize,
    #[serde(default)]
    pub microphone_epoch: u32,
    #[serde(default)]
    pub microphone_source_epoch: u32,
    pub muted: bool,
    pub deafened: bool,
    #[serde(default)]
    pub server_muted: bool,
    #[serde(default)]
    pub server_deafened: bool,
    /// Monotonic moderation boundary, including changes coalesced between rosters.
    #[serde(default)]
    pub publication_revision: u64,
    pub can_speak: bool,
    #[serde(default)]
    pub sharing_screen: bool,
    #[serde(default)]
    pub sharing_audio: bool,
    #[serde(default)]
    pub screen_epoch: u32,
    #[serde(default)]
    pub camera_active: bool,
    #[serde(default)]
    pub camera_epoch: u32,
    /// Remote viewers with a current authorized subscription; local preview is excluded.
    /// Populated only in the connected call's participant snapshots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screen_viewers: Vec<AccountId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub camera_viewers: Vec<AccountId>,
}
/// A visible voice channel's live membership, without joining its media transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRoster {
    pub channel_id: ChannelId,
    pub members: Vec<Participant>,
}
impl Participant {
    pub fn is_muted(&self) -> bool {
        self.muted || self.server_muted
    }
    pub fn is_deafened(&self) -> bool {
        self.deafened || self.server_deafened
    }
    pub fn video_epoch(&self, kind: MediaKind) -> u32 {
        if kind == MediaKind::Camera {
            self.camera_epoch
        } else {
            self.screen_epoch
        }
    }
    pub fn video_active(&self, kind: MediaKind) -> bool {
        if kind == MediaKind::Camera {
            self.camera_active
        } else {
            self.sharing_screen
        }
    }
    pub fn video_viewers(&self, kind: MediaKind) -> &[AccountId] {
        match kind {
            MediaKind::Camera => &self.camera_viewers,
            MediaKind::ScreenVideo => &self.screen_viewers,
            _ => &[],
        }
    }
    /// Profile label, falling back to the username for older servers.
    pub fn display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ServerFrame {
    pub version: u8,
    pub event: ServerEvent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Ends this transport. A current client may reconnect after fresh admission,
    /// preserving self mute/deafen and stopping all screen/camera publishing.
    Moved {
        channel_id: ChannelId,
    },
    MicrophoneRefreshRequired {
        challenge: u32,
    },
    MicrophoneBoundaryAccepted {
        generation: u64,
        /// Listener barrier; a source-only refresh never advances this floor.
        microphone_epoch: u32,
        /// Current publisher privacy revision, matching Participant.microphone_epoch.
        source_epoch: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        challenge: Option<u32>,
        #[serde(default = "accepted_by_default")]
        accepted: bool,
    },
    AudioCommand {
        request_id: crate::RequestId,
        command: crate::remote_audio::Command,
    },
    ScreenTarget {
        #[serde(default = "screen_kind", skip_serializing_if = "is_screen_kind")]
        kind: MediaKind,
        epoch: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layers: Option<crate::screen::LayerTargets>,
        /// Zero pauses video. Legacy publishers use the lowest receiver budget.
        bitrate: u32,
        /// Stream-wide encoder profile every current viewer can decode. Only
        /// sent to publishers that negotiated `screen_profiles`.
        #[serde(
            default,
            skip_serializing_if = "crate::screen::VideoProfile::is_baseline"
        )]
        profile: crate::screen::VideoProfile,
    },
    ScreenKeyframe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layer: Option<u8>,
        #[serde(default = "screen_kind", skip_serializing_if = "is_screen_kind")]
        kind: MediaKind,
        epoch: u32,
    },
    MediaDiagnostics {
        counters: std::collections::BTreeMap<String, u64>,
    },
    Offer {
        #[serde(default)]
        packet_redundancy: bool,
        #[serde(default)]
        remote_audio: bool,
        sdp: String,
        slot: usize,
        can_speak: bool,
        #[serde(default)]
        screen_video: bool,
        #[serde(default)]
        camera_video: bool,
        #[serde(default)]
        screen_feedback: bool,
        #[serde(default)]
        screen_subscriptions: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        screen_layers: bool,
        /// Older desktops reject unknown admin commands as invalid voice events.
        #[serde(default, skip_serializing_if = "is_false")]
        video_diagnostics: bool,
        /// The server accepts soundboard plays and can deliver them.
        #[serde(default, skip_serializing_if = "is_false")]
        soundboard: bool,
        /// The server accepts per-view decoder capabilities and plans one
        /// stream-wide encoder profile from them (no per-viewer encodes).
        #[serde(default, skip_serializing_if = "is_false")]
        screen_profiles: bool,
        /// The server announces planned restarts with `Restarting`.
        #[serde(default, skip_serializing_if = "is_false")]
        restart_notice: bool,
        /// The server accepts `ClientEvent::IceRestart` on this session.
        #[serde(default, skip_serializing_if = "is_false")]
        ice_restart: bool,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
        /// Secret capability bound to this media session; never persist or log it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_token: Option<String>,
    },
    Resumed {},
    /// The server is restarting (a planned deploy or maintenance). The media
    /// session ends; rejoin the same call with a fresh `Join` (full admission and
    /// permission checks) after about `retry_after_ms`, keeping self mute/deafen.
    /// Sent to sessions that negotiated `restart_notice`, and instead of
    /// `ResumeUnavailable` to a resume that reaches a freshly restarted server.
    Restarting {
        retry_after_ms: u64,
    },
    /// An ICE restart offer for the existing session, with fresh relay
    /// credentials. Only sent in reply to `ClientEvent::IceRestart`.
    IceRestart {
        sdp: String,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
    },
    /// The media session no longer exists. Only an explicit new join is allowed:
    /// absence can also mean a moderator ended the call while signaling was lost.
    ResumeUnavailable {},
    Participants {
        members: Vec<Participant>,
    },
    /// A participant played a guild sound. Sent only to clients that opted in.
    Soundboard {
        sound_id: crate::SoundId,
        account_id: AccountId,
    },
    Pong {},
    Revoked {},
    Replaced {},
    Error {
        error: ApiError,
    },
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceStatus {
    #[serde(default)]
    pub own_slot: Option<usize>,
    pub connected: bool,
    pub channel_id: Option<ChannelId>,
    pub message: String,
    pub participants: Vec<Participant>,
    /// The connected call negotiated soundboard playback.
    #[serde(default)]
    pub soundboard: bool,
    /// Reconnecting after a server restart or a lost network path: a calm,
    /// transient state, not an error. Self mute/deafen are kept.
    #[serde(default)]
    pub reconnecting: bool,
    /// Publications a restart reconnection stopped that the UI may offer to
    /// resume (`ScreenVideo` and/or `Camera`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resumable: Vec<MediaKind>,
}
// TURN credentials are short-lived secrets; deliberately no Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    /// Seconds until these relay credentials expire, counted from receipt.
    /// Clients refresh them with an ICE restart before then. Absent for STUN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
}

/// Media ordering is stable on the wire and in negotiated track slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum MediaKind {
    Microphone = 0,
    ScreenVideo = 1,
    SystemAudio = 2,
    Camera = 3,
}
impl MediaKind {
    pub const ALL: [Self; 4] = [
        Self::Microphone,
        Self::ScreenVideo,
        Self::SystemAudio,
        Self::Camera,
    ];
    pub const fn is_video(self) -> bool {
        matches!(self, Self::ScreenVideo | Self::Camera)
    }
    pub const fn view_index(self, slot: usize) -> usize {
        if matches!(self, Self::Camera) {
            ROOM_CAPACITY + slot
        } else {
            slot
        }
    }
    pub const TRACK_COUNT: usize = ROOM_CAPACITY * Self::ALL.len();
    pub const fn publisher_ssrc(self) -> u32 {
        900 + self as u32
    }
    pub const fn payload_type(self) -> u8 {
        if self.is_video() { 125 } else { 111 }
    }
    pub const fn clock_rate(self) -> u32 {
        if self.is_video() { 90_000 } else { 48_000 }
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
            Self::Camera => 40_000,
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
            Self::ScreenVideo | Self::Camera => None,
        }
    }
}
pub fn is_screen_kind(kind: &MediaKind) -> bool {
    *kind == MediaKind::ScreenVideo
}
pub const fn screen_kind() -> MediaKind {
    MediaKind::ScreenVideo
}

#[cfg(test)]
mod media_tests {
    use super::*;
    #[test]
    fn moderation_revision_defaults_for_older_rosters_and_round_trips() {
        let mut participant: Participant = serde_json::from_value(serde_json::json!({
            "account_id":"00000000-0000-0000-0000-000000000001", "username":"member", "slot":0,
            "muted":true, "deafened":false, "can_speak":true
        }))
        .unwrap();
        assert_eq!(participant.publication_revision, 0);
        participant.publication_revision = 2;
        participant.server_deafened = true;
        let decoded: Participant =
            serde_json::from_str(&serde_json::to_string(&participant).unwrap()).unwrap();
        assert_eq!(decoded.publication_revision, 2);
        assert!(decoded.is_deafened() && decoded.muted && !decoded.deafened);
    }

    #[test]
    fn resume_contract_and_legacy_offer_remain_compatible() {
        let wire = serde_json::json!({"version":1,"event":{
            "type":"resume", "token":"test-session", "resume_token":"test-capability", "sequence":2,
            "guild_id":"00000000-0000-0000-0000-000000000001",
            "channel_id":"00000000-0000-0000-0000-000000000002"
        }});
        let frame: ClientFrame = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            &frame.event,
            ClientEvent::Resume { sequence: 2, .. }
        ));
        assert_eq!(serde_json::to_value(frame).unwrap(), wire);
        let legacy: ServerEvent = serde_json::from_value(serde_json::json!({
            "type":"offer","sdp":"test","slot":0,"can_speak":true
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            ServerEvent::Offer {
                resume_token: None,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(ServerEvent::Resumed {}).unwrap(),
            serde_json::json!({"type":"resumed"})
        );
        assert_eq!(
            serde_json::to_value(ServerEvent::ResumeUnavailable {}).unwrap(),
            serde_json::json!({"type":"resume_unavailable"})
        );
    }
    #[test]
    fn join_attempt_is_optional_for_old_clients_and_replacement_is_typed() {
        let legacy = serde_json::json!({"version":1,"event":{
            "type":"join","token":"not-a-real-token",
            "guild_id":"00000000-0000-0000-0000-000000000001",
            "channel_id":"00000000-0000-0000-0000-000000000002"
        }});
        let mut frame: ClientFrame = serde_json::from_value(legacy.clone()).unwrap();
        let ClientEvent::Join { attempt, .. } = &mut frame.event else {
            panic!("join")
        };
        assert!(attempt.is_none());
        *attempt = Some(JoinAttempt {
            call_id: "00000000-0000-0000-0000-000000000003".parse().unwrap(),
            reconnect: true,
        });
        let encoded = serde_json::to_value(&frame).unwrap();
        assert_eq!(encoded["event"]["attempt"]["reconnect"], true);
        let decoded: ClientFrame = serde_json::from_value(encoded).unwrap();
        assert!(matches!(
            decoded.event,
            ClientEvent::Join {
                attempt: Some(JoinAttempt {
                    reconnect: true,
                    ..
                }),
                ..
            }
        ));
        let old: ClientFrame = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(old).unwrap(), legacy);
        assert_eq!(
            serde_json::to_value(ServerEvent::Replaced {}).unwrap(),
            serde_json::json!({"type":"replaced"})
        );
    }
    #[test]
    fn soundboard_is_opt_in_and_omitted_for_older_peers() {
        let legacy: ServerEvent = serde_json::from_value(serde_json::json!({
            "type":"offer","sdp":"test","slot":0,"can_speak":true
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            ServerEvent::Offer {
                soundboard: false,
                ..
            }
        ));
        let answer = ClientEvent::Answer {
            browser_publisher: false,
            packet_redundancy: false,
            remote_audio: false,
            sdp: "test".into(),
            screen_feedback: false,
            screen_subscriptions: false,
            screen_layers: false,
            video_diagnostics: false,
            soundboard: false,
            screen_profiles: false,
            restart_notice: false,
            ice_restart: false,
        };
        let answer_wire = serde_json::to_value(&answer).unwrap();
        assert!(answer_wire.get("soundboard").is_none());
        // Older servers deny unknown answer fields; the default must be omitted.
        assert!(answer_wire.get("screen_profiles").is_none());
        assert!(matches!(
            legacy,
            ServerEvent::Offer {
                screen_profiles: false,
                ..
            }
        ));
        // Older publishers deny unknown target fields: baseline stays omitted
        // and an omitted profile decodes as baseline.
        let target = ServerEvent::ScreenTarget {
            kind: MediaKind::ScreenVideo,
            epoch: 4,
            layers: None,
            bitrate: 2_000_000,
            profile: crate::screen::VideoProfile::H264Baseline,
        };
        let target_wire = serde_json::to_value(&target).unwrap();
        assert_eq!(
            target_wire,
            serde_json::json!({"type":"screen_target","epoch":4,"bitrate":2_000_000})
        );
        assert!(matches!(
            serde_json::from_value(target_wire).unwrap(),
            ServerEvent::ScreenTarget {
                profile: crate::screen::VideoProfile::H264Baseline,
                ..
            }
        ));
        let high = serde_json::to_value(ServerEvent::ScreenTarget {
            kind: MediaKind::ScreenVideo,
            epoch: 4,
            layers: None,
            bitrate: 2_000_000,
            profile: crate::screen::VideoProfile::H264High,
        })
        .unwrap();
        assert_eq!(high["profile"], "h264_high");
        let id = "00000000-0000-0000-0000-000000000001";
        let wire = serde_json::json!({"version":1,"event":{"type":"soundboard","sound_id":id}});
        let frame: ClientFrame = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), wire);
        let played = serde_json::to_value(ServerEvent::Soundboard {
            sound_id: id.parse().unwrap(),
            account_id: id.parse().unwrap(),
        })
        .unwrap();
        assert_eq!(
            played,
            serde_json::json!({"type":"soundboard","sound_id":id,"account_id":id})
        );
    }
    #[test]
    fn restart_and_ice_restart_are_opt_in_and_compatible_with_older_peers() {
        // Older servers deny unknown answer fields: defaults stay omitted.
        let answer = |restart_notice, ice_restart| ClientEvent::Answer {
            browser_publisher: false,
            packet_redundancy: false,
            remote_audio: false,
            sdp: "test".into(),
            screen_feedback: false,
            screen_subscriptions: false,
            screen_layers: false,
            video_diagnostics: false,
            soundboard: false,
            screen_profiles: false,
            restart_notice,
            ice_restart,
        };
        let legacy = serde_json::to_value(answer(false, false)).unwrap();
        assert!(legacy.get("restart_notice").is_none() && legacy.get("ice_restart").is_none());
        let opted = serde_json::to_value(answer(true, true)).unwrap();
        assert_eq!(opted["restart_notice"], true);
        assert_eq!(opted["ice_restart"], true);
        // Older servers' offers decode without either capability.
        let offer: ServerEvent = serde_json::from_value(serde_json::json!({
            "type":"offer","sdp":"test","slot":0,"can_speak":true,
            "ice_servers":[{"urls":["turn:example.test"],"username":"1:a","credential":"c"}]
        }))
        .unwrap();
        let ServerEvent::Offer {
            restart_notice,
            ice_restart,
            ice_servers,
            ..
        } = offer
        else {
            panic!("offer");
        };
        assert!(!restart_notice && !ice_restart);
        assert_eq!(ice_servers[0].expires_in_secs, None);
        let server = IceServer {
            urls: vec!["turn:example.test".into()],
            username: "1:a".into(),
            credential: "c".into(),
            expires_in_secs: Some(3600),
        };
        assert_eq!(
            serde_json::to_value(&server).unwrap()["expires_in_secs"],
            3600
        );
        assert_eq!(
            serde_json::to_value(ServerEvent::Restarting {
                retry_after_ms: 1500
            })
            .unwrap(),
            serde_json::json!({"type":"restarting","retry_after_ms":1500})
        );
        let restart = serde_json::json!({"version":1,"event":{"type":"ice_restart"}});
        let frame: ClientFrame = serde_json::from_value(restart.clone()).unwrap();
        assert!(matches!(frame.event, ClientEvent::IceRestart {}));
        assert_eq!(serde_json::to_value(frame).unwrap(), restart);
        let reply =
            serde_json::json!({"version":1,"event":{"type":"ice_restart_answer","sdp":"x"}});
        let frame: ClientFrame = serde_json::from_value(reply).unwrap();
        assert!(matches!(frame.event, ClientEvent::IceRestartAnswer { .. }));
        // An unknown answer field is still rejected, as older servers do.
        assert!(
            serde_json::from_value::<ClientFrame>(serde_json::json!({"version":1,"event":{
                "type":"ice_restart_answer","sdp":"x","extra":true}}))
            .is_err()
        );
        let status: VoiceStatus = serde_json::from_value(serde_json::json!({
            "connected":false,"channel_id":null,"message":"","participants":[]
        }))
        .unwrap();
        assert!(!status.reconnecting && status.resumable.is_empty());
    }

    #[test]
    fn media_mappings_round_trip_and_reject_unknown_sources() {
        assert_eq!(
            MediaKind::ALL.map(MediaKind::publisher_ssrc),
            [900, 901, 902, 903]
        );
        assert_eq!(
            MediaKind::ALL.map(MediaKind::ssrc_base),
            [10_000, 20_000, 30_000, 40_000]
        );
        assert_eq!(
            MediaKind::ALL.map(MediaKind::payload_type),
            [111, 125, 111, 125]
        );
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
        assert_eq!(MediaKind::from_publisher_ssrc(904), None);
    }
}
