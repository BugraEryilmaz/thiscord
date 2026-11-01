//! Desktop overlay IPC contains presentation metadata only.
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OverlaySnapshot {
    pub participants: Vec<OverlayParticipant>,
    /// Older desktops omit layout; the defaults match their fixed top-left roster.
    #[serde(default)]
    pub layout: OverlayLayout,
}

/// Presentation state the overlay WebView needs to mirror native hit testing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayLayout {
    pub corner: OverlayCorner,
    pub visibility_button: bool,
    /// Collapsed for this app session; only the optional button stays visible.
    pub hidden: bool,
    /// Positioning mode: the whole overlay accepts input so it can be dragged.
    pub unlocked: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayParticipant {
    #[serde(default)]
    pub is_self: bool,
    pub account_id: crate::AccountId,
    pub name: String,
    #[serde(default)]
    pub avatar_id: Option<crate::AvatarId>,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
}

/// Display corner the overlay is anchored to. `ALL` is the rotation order used
/// by the "next corner" shortcut: counterclockwise from the top left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayCorner {
    #[default]
    TopLeft,
    BottomLeft,
    BottomRight,
    TopRight,
}
impl OverlayCorner {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::BottomLeft,
        Self::BottomRight,
        Self::TopRight,
    ];
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
    pub fn bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }
    pub fn right(self) -> bool {
        matches!(self, Self::BottomRight | Self::TopRight)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::BottomLeft => "Bottom left",
            Self::BottomRight => "Bottom right",
            Self::TopRight => "Top right",
        }
    }
}

/// Largest stored edge distance in logical pixels; positions are also clamped
/// to the current display when applied.
pub const MAX_OFFSET: u16 = 4096;
const MAX_DISPLAY_NAME: usize = 256;

/// Persisted per computer. Shortcuts live with the other global shortcuts so
/// registration can reject conflicts in one place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlaySettings {
    pub enabled: bool,
    pub corner: OverlayCorner,
    /// Logical pixels from the left or right display edge, matching `corner`.
    pub offset_x: u16,
    /// Logical pixels from the top or bottom display edge, matching `corner`.
    pub offset_y: u16,
    /// Native display name; `None` follows the display containing Thiscord.
    pub display: Option<String>,
    /// Small hide/show button that stays on the overlay while it is collapsed.
    pub visibility_button: bool,
}
impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            corner: OverlayCorner::TopLeft,
            offset_x: 24,
            offset_y: 48,
            display: None,
            visibility_button: false,
        }
    }
}
impl OverlaySettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.offset_x > MAX_OFFSET || self.offset_y > MAX_OFFSET {
            return Err("Overlay distance from the display edge is too large");
        }
        if self
            .display
            .as_ref()
            .is_some_and(|d| d.is_empty() || d.len() > MAX_DISPLAY_NAME)
        {
            return Err("Invalid overlay display");
        }
        Ok(())
    }
}

/// Session actions shared by global shortcuts, overlay buttons and Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayAction {
    ToggleVisibility,
    NextCorner,
    ToggleUnlock,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayDisplay {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverlayStatus {
    pub settings: OverlaySettings,
    pub layout: OverlayLayout,
    /// Displays currently connected, in logical pixels.
    pub displays: Vec<OverlayDisplay>,
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

    #[test]
    fn corners_rotate_counterclockwise_from_top_left() {
        use super::OverlayCorner::*;
        assert_eq!(TopLeft.next(), BottomLeft);
        assert_eq!(BottomLeft.next(), BottomRight);
        assert_eq!(BottomRight.next(), TopRight);
        assert_eq!(TopRight.next(), TopLeft);
        assert!(BottomLeft.bottom() && !BottomLeft.right());
        assert!(TopRight.right() && !TopRight.bottom());
    }

    #[test]
    fn settings_have_stable_defaults_and_bounded_fields() {
        let older: super::OverlaySettings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!older.enabled);
        assert_eq!(older.corner, super::OverlayCorner::TopLeft);
        assert_eq!((older.offset_x, older.offset_y), (24, 48));
        assert!(older.validate().is_ok());
        assert!(serde_json::from_str::<super::OverlaySettings>(r#"{"path":"x"}"#).is_err());
        let corner: super::OverlaySettings =
            serde_json::from_str(r#"{"corner":"bottom_right"}"#).unwrap();
        assert_eq!(corner.corner, super::OverlayCorner::BottomRight);
        for invalid in [
            super::OverlaySettings {
                offset_x: super::MAX_OFFSET + 1,
                ..Default::default()
            },
            super::OverlaySettings {
                display: Some(String::new()),
                ..Default::default()
            },
            super::OverlaySettings {
                display: Some("x".repeat(257)),
                ..Default::default()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        let snapshot: super::OverlaySnapshot =
            serde_json::from_str(r#"{"participants":[]}"#).unwrap();
        assert_eq!(snapshot.layout, super::OverlayLayout::default());
    }
}
