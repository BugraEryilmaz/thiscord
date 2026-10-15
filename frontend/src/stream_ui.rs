//! Local viewing intent is bound to the publisher and share generation.
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    voice::{Participant, VoiceStatus},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub guild: GuildId,
    pub channel: ChannelId,
    pub owner: AccountId,
    pub epoch: u32,
}
impl Selection {
    pub fn source(self, guild: GuildId, voice: &VoiceStatus) -> Option<&Participant> {
        if guild != self.guild || !voice.connected || voice.channel_id != Some(self.channel) {
            return None;
        }
        voice.participants.iter().find(|p| {
            p.account_id == self.owner && p.screen_epoch == self.epoch && p.sharing_screen
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn selection_never_follows_reused_slots_other_calls_or_restarted_shares() {
        let mut voice:VoiceStatus=serde_json::from_value(json!({"connected":true,"channel_id":"00000000-0000-0000-0000-000000000001","message":"","participants":[{"account_id":"00000000-0000-0000-0000-000000000002","username":"publisher","slot":1,"muted":false,"deafened":false,"can_speak":true,"sharing_screen":true,"screen_epoch":3}]})).unwrap();
        let guild = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        let selected = Selection {
            guild,
            channel: voice.channel_id.unwrap(),
            owner: voice.participants[0].account_id,
            epoch: 3,
        };
        assert!(selected.source(guild, &voice).is_some());
        voice.participants[0].slot = 5;
        assert!(selected.source(guild, &voice).is_some());
        voice.participants[0].screen_epoch = 4;
        assert!(selected.source(guild, &voice).is_none());
        voice.participants[0].screen_epoch = 3;
        voice.participants[0].sharing_screen = false;
        assert!(selected.source(guild, &voice).is_none());
        voice.participants[0].sharing_screen = true;
        voice.participants[0].account_id = guild.to_string().parse().unwrap();
        assert!(selected.source(guild, &voice).is_none());
        voice.participants[0].account_id = selected.owner;
        assert!(
            selected
                .source(selected.channel.to_string().parse().unwrap(), &voice)
                .is_none()
        );
        voice.channel_id = None;
        assert!(selected.source(guild, &voice).is_none());
    }
}
