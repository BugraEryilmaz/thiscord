use thiscord_shared::{
    audio::AudioStatus,
    overlay::{OverlayParticipant, OverlaySnapshot},
    voice::VoiceStatus,
};

pub fn snapshot(voice: &VoiceStatus, audio: &AudioStatus) -> OverlaySnapshot {
    if !voice.connected || voice.channel_id.is_none() {
        return OverlaySnapshot::default();
    }
    OverlaySnapshot {
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
                    account_id: p.account_id,
                    name: p.display_name().to_owned(),
                    speaking: audio.running && activity && p.can_speak && !p.muted && !p.deafened,
                    muted: p.muted || !p.can_speak,
                    deafened: p.deafened,
                }
            })
            .collect(),
    }
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
}
