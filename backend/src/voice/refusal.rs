//! Why ingress stopped accepting a connection's media. Recorded once where
//! media is inspected (both transports), then logged and reported to the client
//! by the signaling owner instead of ending the call without a reason.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use thiscord_shared::{ApiError, ErrorCode, RequestId, screen, voice::MediaKind};

/// Largest RTP payload either transport accepts.
pub(super) const MAX_PAYLOAD: usize = 1500;
/// Microphone and shared audio: 50 packets/s plus slack; no real-time Opus
/// sender approaches either limit.
const AUDIO_PACKETS_PER_SECOND: usize = 100;
const AUDIO_BYTES_PER_SECOND: usize = 150_000;

/// Per-track ingress limits for one rate window, shared by both transports.
/// `packets` and `bytes` already include the packet being checked.
pub(super) fn ingress_limit(
    kind: MediaKind,
    packets: usize,
    bytes: usize,
    payload: usize,
) -> Option<Reason> {
    if payload > MAX_PAYLOAD {
        return Some(Reason::OversizedPacket);
    }
    let (max_packets, max_bytes) = if kind.is_video() {
        (screen::MAX_PACKETS_PER_SECOND, screen::MAX_BYTES_PER_SECOND)
    } else {
        (AUDIO_PACKETS_PER_SECOND, AUDIO_BYTES_PER_SECOND)
    };
    (packets > max_packets || bytes > max_bytes).then_some(if kind == MediaKind::Microphone {
        Reason::MicrophoneRate
    } else {
        Reason::MediaRate
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Reason {
    /// A track the offer did not negotiate: unknown SSRC/line, codec or a duplicate.
    UnexpectedTrack = 1,
    /// More microphone packets or bytes per second than any real-time sender.
    MicrophoneRate = 2,
    /// Screen, camera or shared audio above its rate budget.
    MediaRate = 3,
    OversizedPacket = 4,
}

impl Reason {
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::UnexpectedTrack,
            2 => Self::MicrophoneRate,
            3 => Self::MediaRate,
            4 => Self::OversizedPacket,
            _ => return None,
        })
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::UnexpectedTrack => "unexpected_track",
            Self::MicrophoneRate => "microphone_rate",
            Self::MediaRate => "media_rate",
            Self::OversizedPacket => "oversized_packet",
        }
    }
    /// Final for this connection: a client fault that a resume or automatic
    /// rejoin would repeat, so the client must not retry on its own.
    pub fn error(self, request_id: RequestId) -> ApiError {
        let message = match self {
            Self::UnexpectedTrack => {
                "Voice stopped because the app sent media the server did not expect. Update Thiscord and join again."
            }
            Self::MicrophoneRate => {
                "Voice stopped because your microphone sent audio faster than real time. Check or change your microphone, then join again."
            }
            Self::MediaRate => {
                "Voice stopped because your screen share, camera or shared audio sent more data than allowed. Join again."
            }
            Self::OversizedPacket => {
                "Voice stopped because the app sent a media packet that was too large. Join again."
            }
        };
        ApiError {
            code: ErrorCode::BadRequest,
            message: message.into(),
            request_id,
            fields: vec![],
        }
    }
}

#[derive(Default)]
pub(super) struct Refusal(AtomicU8);

impl Refusal {
    /// Records the first reason, then stops media. The reason is visible to
    /// anyone who observes `active == false` because of it.
    pub fn refuse(&self, active: &AtomicBool, reason: Reason) {
        let _ = self
            .0
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        active.store(false, Ordering::Release);
    }
    pub fn reason(&self) -> Option<Reason> {
        Reason::from_code(self.0.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_reason_is_kept_and_media_stops() {
        let refusal = Refusal::default();
        let active = AtomicBool::new(true);
        assert_eq!(refusal.reason(), None);
        refusal.refuse(&active, Reason::MicrophoneRate);
        refusal.refuse(&active, Reason::UnexpectedTrack);
        assert!(!active.load(Ordering::Acquire));
        assert_eq!(refusal.reason(), Some(Reason::MicrophoneRate));
    }

    #[test]
    fn ingress_limits_name_the_offending_media() {
        let mic = MediaKind::Microphone;
        assert_eq!(ingress_limit(mic, 100, 100 * 1100, 1100), None);
        assert_eq!(
            ingress_limit(mic, 101, 101 * 80, 80),
            Some(Reason::MicrophoneRate)
        );
        assert_eq!(
            ingress_limit(mic, 2, 150_001, 1000),
            Some(Reason::MicrophoneRate)
        );
        assert_eq!(
            ingress_limit(mic, 1, 1501, 1501),
            Some(Reason::OversizedPacket)
        );
        assert_eq!(
            ingress_limit(MediaKind::SystemAudio, 101, 101 * 80, 80),
            Some(Reason::MediaRate)
        );
        let video = MediaKind::ScreenVideo;
        assert_eq!(ingress_limit(video, 101, 101 * 1200, 1200), None);
        assert_eq!(
            ingress_limit(video, screen::MAX_PACKETS_PER_SECOND + 1, 0, 1200),
            Some(Reason::MediaRate)
        );
    }

    #[test]
    fn refusals_are_final_client_errors() {
        let id = RequestId::from_uuid(uuid::Uuid::nil());
        for reason in [
            Reason::UnexpectedTrack,
            Reason::MicrophoneRate,
            Reason::MediaRate,
            Reason::OversizedPacket,
        ] {
            assert_eq!(Reason::from_code(reason as u8), Some(reason));
            let error = reason.error(id);
            // Conflict/RateLimited/ServiceUnavailable/InternalError are retried
            // automatically by clients; a refusal must not be.
            assert_eq!(error.code, ErrorCode::BadRequest);
            assert_eq!(error.request_id, id);
            assert!(error.message.starts_with("Voice stopped because"));
        }
    }
}
