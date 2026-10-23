use thiscord_shared::{
    AccountId, ChannelId,
    audio::AudioStatus,
    overlay::{OverlayParticipant, OverlaySnapshot},
    voice::VoiceStatus,
};

/// Reuse overlay activity rules in channel views, without attributing another
/// call's audio or screen-share audio to a visible participant.
pub fn speaking_in_channel(
    voice: &VoiceStatus,
    audio: Option<&AudioStatus>,
    channel: ChannelId,
) -> std::collections::BTreeSet<AccountId> {
    if voice.channel_id != Some(channel) {
        return Default::default();
    }
    audio
        .map(|audio| {
            snapshot(voice, audio)
                .participants
                .into_iter()
                .filter(|p| p.speaking)
                .map(|p| p.account_id)
                .collect()
        })
        .unwrap_or_default()
}

pub fn snapshot(voice: &VoiceStatus, audio: &AudioStatus) -> OverlaySnapshot {
    if !voice.connected || voice.channel_id.is_none() {
        return OverlaySnapshot::default();
    }
    let mut snapshot = OverlaySnapshot {
        participants: voice
            .participants
            .iter()
            .map(|p| {
                let activity = if voice.own_slot == Some(p.slot) {
                    audio.transmitting
                } else {
                    audio.streams.iter().any(|s| {
                        s.speaking
                            && s.target
                                .is_some_and(|t| t.account_id == p.account_id && !t.shared_audio)
                    })
                };
                OverlayParticipant {
                    is_self: voice.own_slot == Some(p.slot),
                    account_id: p.account_id,
                    name: p.display_name().to_owned(),
                    avatar_id: p.avatar_id,
                    speaking: audio.running
                        && activity
                        && p.can_speak
                        && !p.is_muted()
                        && !p.is_deafened(),
                    muted: p.is_muted() || !p.can_speak,
                    deafened: p.is_deafened(),
                }
            })
            .collect(),
    };
    snapshot.participants.sort_by_key(|p| !p.is_self);
    snapshot
}

/// Matches the fixed local row in style.css; all other pixels pass through.
pub fn over_self_controls(x: f64, y: f64) -> bool {
    (6.0..274.0).contains(&x) && (6.0..58.0).contains(&y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (VoiceStatus, AudioStatus) {
        let voice = serde_json::from_value(json!({
            "connected": true, "channel_id": "00000000-0000-0000-0000-000000000001",
            "own_slot": 0, "message": "Connected", "participants": [
                {"account_id": "00000000-0000-0000-0000-000000000002", "username": "self",
                 "slot": 0, "muted": false, "deafened": false, "can_speak": true},
                {"account_id": "00000000-0000-0000-0000-000000000003", "username": "other",
                 "slot": 1, "muted": false, "deafened": false, "can_speak": true}
            ]
        }))
        .unwrap();
        let audio = serde_json::from_value(json!({
            "running": true, "input_level": 0.1, "transmitting": true,
            "message": "", "dropped_samples": 0, "underrun_samples": 0,
            "streams": [{"id": "1", "label": "other", "volume": 0.0, "speaking": true,
                "target": {"guild_id": "00000000-0000-0000-0000-000000000004",
                    "account_id": "00000000-0000-0000-0000-000000000003", "shared_audio": false}}]
        }))
        .unwrap();
        (voice, audio)
    }

    #[test]
    fn channel_activity_is_scoped_to_the_connected_call_and_excludes_screen_audio() {
        let (mut voice, mut audio) = fixture();
        let channel = voice.channel_id.unwrap();
        assert_eq!(speaking_in_channel(&voice, Some(&audio), channel).len(), 2);
        let other = "00000000-0000-0000-0000-000000000099".parse().unwrap();
        assert!(speaking_in_channel(&voice, Some(&audio), other).is_empty());
        assert!(speaking_in_channel(&voice, None, channel).is_empty());
        audio.streams[0].target.as_mut().unwrap().shared_audio = true;
        assert_eq!(speaking_in_channel(&voice, Some(&audio), channel).len(), 1);
        voice.participants[0].muted = true;
        assert!(speaking_in_channel(&voice, Some(&audio), channel).is_empty());
        voice.connected = false;
        assert!(speaking_in_channel(&voice, Some(&audio), channel).is_empty());
    }

    #[test]
    fn only_own_row_is_interactive_even_after_roster_reordering() {
        let (mut voice, audio) = fixture();
        voice.participants.reverse();
        let view = snapshot(&voice, &audio);
        assert!(view.participants[0].is_self);
        assert!(!view.participants[1].is_self);
        assert_eq!(view.participants[0].name, "self");
        assert!(over_self_controls(6.0, 6.0));
        assert!(over_self_controls(273.9, 57.9));
        for (x, y) in [
            (5.9, 6.0),
            (274.0, 6.0),
            (6.0, 58.0),
            (6.0, 68.0),
            (f64::NAN, 6.0),
        ] {
            assert!(!over_self_controls(x, y));
        }
        voice.own_slot = None;
        assert!(
            snapshot(&voice, &audio)
                .participants
                .iter()
                .all(|p| !p.is_self)
        );
    }

    #[test]
    fn activity_matches_identity_and_excludes_screen_audio() {
        let (voice, mut audio) = fixture();
        assert!(
            snapshot(&voice, &audio)
                .participants
                .iter()
                .all(|p| p.speaking)
        );
        audio.streams[0].target.as_mut().unwrap().shared_audio = true;
        assert!(!snapshot(&voice, &audio).participants[1].speaking);
        audio.streams[0].target.as_mut().unwrap().shared_audio = false;
        audio.streams[0].target.as_mut().unwrap().account_id = voice.participants[0].account_id;
        assert!(!snapshot(&voice, &audio).participants[1].speaking);
    }

    #[test]
    fn restrictions_and_disconnect_override_activity() {
        let (mut voice, mut audio) = fixture();
        voice.participants[0].muted = true;
        voice.participants[1].can_speak = false;
        assert!(
            snapshot(&voice, &audio)
                .participants
                .iter()
                .all(|p| !p.speaking)
        );
        voice.participants[0].muted = false;
        audio.transmitting = false;
        assert!(!snapshot(&voice, &audio).participants[0].speaking);
        audio.running = false;
        assert!(
            snapshot(&voice, &audio)
                .participants
                .iter()
                .all(|p| !p.speaking)
        );
        voice.connected = false;
        assert!(snapshot(&voice, &audio).participants.is_empty());
    }

    #[test]
    fn profile_picture_changes_and_removal_reach_the_overlay() {
        let (mut voice, audio) = fixture();
        assert!(snapshot(&voice, &audio).participants[0].avatar_id.is_none());
        let id = "00000000-0000-0000-0000-000000000005".parse().unwrap();
        voice.participants[0].avatar_id = Some(id);
        assert_eq!(snapshot(&voice, &audio).participants[0].avatar_id, Some(id));
        assert!(snapshot(&voice, &audio).participants[1].avatar_id.is_none());
        voice.participants[0].avatar_id = None;
        assert!(snapshot(&voice, &audio).participants[0].avatar_id.is_none());
    }
}
