//! Desktop overlay IPC contains presentation metadata only.
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OverlaySnapshot {
    pub participants: Vec<OverlayParticipant>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OverlayParticipant {
    pub account_id: crate::AccountId,
    pub name: String,
    #[serde(default)]
    pub avatar_id: Option<crate::AvatarId>,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn older_voice_and_audio_metadata_default_to_no_activity() {
        let voice: crate::voice::VoiceStatus = serde_json::from_value(serde_json::json!({
            "connected": false, "channel_id": null, "message": "", "participants": []
        }))
        .unwrap();
        assert!(voice.own_slot.is_none());
        let stream: crate::audio::StreamLevel = serde_json::from_value(serde_json::json!({
            "id": "0", "label": "Speaker", "volume": 1.0
        }))
        .unwrap();
        assert!(!stream.speaking);
    }
}
