use thiscord_shared::{
    AccountId, ChannelId,
    audio::AudioStatus,
    overlay::{
        MAX_OFFSET, OverlayCorner, OverlayLayout, OverlayParticipant, OverlaySettings,
        OverlaySnapshot,
    },
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
        layout: OverlayLayout::default(),
    };
    snapshot.participants.sort_by_key(|p| !p.is_self);
    snapshot
}

/// Logical overlay window size and the fixed geometry mirrored in style.css.
pub const WIDTH: f64 = 280.0;
pub const HEIGHT: f64 = 560.0;
const PAD: f64 = 6.0;
const BUTTON: f64 = 24.0;
const GAP: f64 = 10.0;
const ROW: f64 = 52.0;

/// Regions that accept mouse input while the overlay is locked; all other
/// pixels pass through to the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    VisibilityButton,
    SelfRow,
}

/// Hit test in logical window coordinates. The visibility button sits in the
/// anchored corner; your own row is the roster entry closest to that corner.
pub fn hit(layout: OverlayLayout, has_self: bool, x: f64, y: f64) -> Option<Hit> {
    let corner = layout.corner;
    // Distance from the anchored vertical edge, so one rule covers all corners.
    let from_edge = if corner.bottom() { HEIGHT - y } else { y };
    let mut start = PAD;
    if layout.visibility_button {
        let from_side = if corner.right() { WIDTH - x } else { x };
        if (PAD..PAD + BUTTON).contains(&from_side) && (start..start + BUTTON).contains(&from_edge)
        {
            return Some(Hit::VisibilityButton);
        }
        start += BUTTON + GAP;
    }
    (!layout.hidden
        && has_self
        && (PAD..WIDTH - PAD).contains(&x)
        && (start..start + ROW).contains(&from_edge))
    .then_some(Hit::SelfRow)
}

/// A display area in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}
impl Area {
    fn logical(self) -> (f64, f64) {
        (
            f64::from(self.width) / self.scale,
            f64::from(self.height) / self.scale,
        )
    }
    pub fn contains(self, x: i32, y: i32) -> bool {
        (i64::from(self.x)..i64::from(self.x) + i64::from(self.width)).contains(&i64::from(x))
            && (i64::from(self.y)..i64::from(self.y) + i64::from(self.height))
                .contains(&i64::from(y))
    }
}

/// Physical window origin for the saved corner and edge distances, kept fully
/// inside the display area.
pub fn origin(settings: &OverlaySettings, area: Area) -> (i32, i32) {
    let (width, height) = area.logical();
    let x = f64::from(settings.offset_x).min((width - WIDTH).max(0.0));
    let y = f64::from(settings.offset_y).min((height - HEIGHT).max(0.0));
    let x = if settings.corner.right() {
        width - WIDTH - x
    } else {
        x
    };
    let y = if settings.corner.bottom() {
        height - HEIGHT - y
    } else {
        y
    };
    (
        area.x + (x.max(0.0) * area.scale).round() as i32,
        area.y + (y.max(0.0) * area.scale).round() as i32,
    )
}

/// Inverse of `origin` after a drag: the window center selects the nearest
/// corner, and distances are measured from that corner's edges.
pub fn placement(window: (i32, i32), area: Area) -> (OverlayCorner, u16, u16) {
    let (width, height) = area.logical();
    let left = f64::from(window.0 - area.x) / area.scale;
    let top = f64::from(window.1 - area.y) / area.scale;
    let right = left + WIDTH / 2.0 > width / 2.0;
    let bottom = top + HEIGHT / 2.0 > height / 2.0;
    let corner = match (bottom, right) {
        (false, false) => OverlayCorner::TopLeft,
        (true, false) => OverlayCorner::BottomLeft,
        (true, true) => OverlayCorner::BottomRight,
        (false, true) => OverlayCorner::TopRight,
    };
    let x = if right { width - WIDTH - left } else { left };
    let y = if bottom { height - HEIGHT - top } else { top };
    let clamp = |value: f64| value.round().clamp(0.0, f64::from(MAX_OFFSET)) as u16;
    (corner, clamp(x), clamp(y))
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
        let top = OverlayLayout::default();
        assert_eq!(hit(top, true, 6.0, 6.0), Some(Hit::SelfRow));
        assert_eq!(hit(top, true, 273.9, 57.9), Some(Hit::SelfRow));
        for (x, y) in [
            (5.9, 6.0),
            (274.0, 6.0),
            (6.0, 58.0),
            (6.0, 68.0),
            (f64::NAN, 6.0),
        ] {
            assert_eq!(hit(top, true, x, y), None);
        }
        assert_eq!(hit(top, false, 6.0, 6.0), None);
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

    #[test]
    fn hit_regions_follow_the_anchored_corner_and_visibility_button() {
        let layout = |corner, visibility_button, hidden| OverlayLayout {
            corner,
            visibility_button,
            hidden,
            unlocked: false,
        };
        let bottom_right = layout(OverlayCorner::BottomRight, false, false);
        assert_eq!(hit(bottom_right, true, 6.0, 6.0), None);
        assert_eq!(
            hit(bottom_right, true, 140.0, HEIGHT - 6.0 - 1.0),
            Some(Hit::SelfRow)
        );
        assert_eq!(hit(bottom_right, true, 140.0, HEIGHT - 58.0 - 1.0), None);

        // The button takes the corner; the roster starts after it.
        let button = layout(OverlayCorner::TopLeft, true, false);
        assert_eq!(hit(button, true, 10.0, 10.0), Some(Hit::VisibilityButton));
        assert_eq!(hit(button, true, 40.0, 10.0), None);
        assert_eq!(hit(button, true, 40.0, 40.0), Some(Hit::SelfRow));
        let right = layout(OverlayCorner::TopRight, true, false);
        assert_eq!(
            hit(right, true, WIDTH - 10.0, 10.0),
            Some(Hit::VisibilityButton)
        );
        assert_eq!(hit(right, true, 10.0, 10.0), None);
        let bottom = layout(OverlayCorner::BottomLeft, true, false);
        assert_eq!(
            hit(bottom, true, 10.0, HEIGHT - 10.0),
            Some(Hit::VisibilityButton)
        );

        // Collapsed: only the button accepts input.
        let hidden = layout(OverlayCorner::TopLeft, true, true);
        assert_eq!(hit(hidden, true, 10.0, 10.0), Some(Hit::VisibilityButton));
        assert_eq!(hit(hidden, true, 40.0, 40.0), None);
        assert_eq!(
            hit(layout(OverlayCorner::TopLeft, false, true), true, 6.0, 6.0),
            None
        );
    }

    #[test]
    fn positions_round_trip_through_every_corner_and_stay_on_screen() {
        let area = Area {
            x: -1920,
            y: 40,
            width: 3840,
            height: 2100,
            scale: 2.0,
        };
        for corner in OverlayCorner::ALL {
            let settings = OverlaySettings {
                corner,
                offset_x: 30,
                offset_y: 70,
                ..Default::default()
            };
            let window = origin(&settings, area);
            assert!(area.contains(window.0, window.1));
            assert_eq!(placement(window, area), (corner, 30, 70));
        }
        let top_left = origin(&OverlaySettings::default(), area);
        assert_eq!(top_left, (-1920 + 48, 40 + 96));
        let bottom_right = origin(
            &OverlaySettings {
                corner: OverlayCorner::BottomRight,
                offset_x: 0,
                offset_y: 0,
                ..Default::default()
            },
            area,
        );
        assert_eq!(bottom_right, (-1920 + 3840 - 560, 40 + 2100 - 1120));
        // Oversized distances clamp to the display instead of leaving it.
        let far = OverlaySettings {
            offset_x: MAX_OFFSET,
            offset_y: MAX_OFFSET,
            ..Default::default()
        };
        assert_eq!(origin(&far, area), (-1920 + 3840 - 560, 40 + 2100 - 1120));
        // Dragging past an edge stores a zero distance.
        assert_eq!(
            placement((-1920 - 50, 0), area),
            (OverlayCorner::TopLeft, 0, 0)
        );
    }
}
