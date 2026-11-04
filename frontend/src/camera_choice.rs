//! Pure rules for choosing a webcam, shared by the desktop WebView camera and
//! the `/app` browser client. The WASM client (`cameras.rs`) owns
//! `enumerateDevices`, `getUserMedia` and storage; listing, validation of the
//! saved preference, the missing-device fallback and its one-time notice stay
//! target-independent so they are unit tested natively.
use serde::{Deserialize, Serialize};

/// Device-local preference (localStorage of the WebView or site origin).
pub const STORAGE_KEY: &str = "thiscord.camera.v1";
/// Browsers use opaque per-origin IDs; anything longer is not one of them.
pub const MAX_DEVICE_ID: usize = 2048;
pub const MAX_LABEL: usize = 256;
/// Upper bound for listed cameras, so a misbehaving driver cannot flood menus.
pub const MAX_CAMERAS: usize = 16;

/// The remembered camera. The label is kept only to name a camera that is
/// currently disconnected; selection always uses the ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedCamera {
    pub id: String,
    #[serde(default)]
    pub label: String,
}
impl SavedCamera {
    pub fn new(id: &str, label: &str) -> Option<Self> {
        let saved = Self {
            id: id.to_owned(),
            label: clip(label.trim(), MAX_LABEL),
        };
        saved.valid().then_some(saved)
    }
    fn valid(&self) -> bool {
        !self.id.trim().is_empty() && self.id.len() <= MAX_DEVICE_ID
    }
    /// A stored value; anything unreadable or out of bounds is ignored.
    pub fn decode(text: &str) -> Option<Self> {
        let saved: Self = serde_json::from_str(text).ok()?;
        Self::new(&saved.id, &saved.label)
    }
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// One selectable video input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDevice {
    pub id: String,
    /// Empty when the browser hides names; the UI then shows "Camera {index}".
    pub label: String,
    /// One-based position among the listed cameras.
    pub index: usize,
}

/// The current `enumerateDevices` result for video inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CameraList {
    pub devices: Vec<CameraDevice>,
    /// Video inputs reported at all, including ones whose IDs are hidden.
    pub present: usize,
    /// Names are visible, i.e. the origin has camera permission. Only then
    /// are the IDs authoritative: before permission some browsers hide them
    /// or report temporary ones.
    pub labeled: bool,
}
impl CameraList {
    /// Builds the list from `(deviceId, label)` pairs of video inputs.
    pub fn new<I, S>(inputs: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: AsRef<str>,
    {
        let mut list = Self::default();
        for (id, label) in inputs {
            list.present += 1;
            let (id, label) = (id.as_ref(), label.as_ref().trim());
            list.labeled |= !label.is_empty();
            if id.is_empty()
                || id.len() > MAX_DEVICE_ID
                || list.devices.len() >= MAX_CAMERAS
                || list.devices.iter().any(|d| d.id == id)
            {
                continue;
            }
            list.devices.push(CameraDevice {
                id: id.to_owned(),
                label: clip(label, MAX_LABEL),
                index: list.devices.len() + 1,
            });
        }
        list
    }
    pub fn find(&self, id: &str) -> Option<&CameraDevice> {
        self.devices.iter().find(|d| d.id == id)
    }
    /// Permission is still needed before cameras can be told apart.
    pub fn needs_permission(&self) -> bool {
        self.present > 0 && !self.labeled
    }
}

/// How to open the camera for the saved preference and the current list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No preference: the system default.
    Default,
    /// Request this device exactly; fall back to the default if it is gone.
    Device(String),
    /// The saved camera is known to be disconnected: open the system default
    /// and announce it. The preference itself is kept.
    Missing,
}
pub fn plan(saved: Option<&SavedCamera>, list: &CameraList) -> Plan {
    match saved {
        None => Plan::Default,
        Some(saved) if list.labeled && list.find(&saved.id).is_none() => Plan::Missing,
        // Unlabeled lists are not authoritative: try the exact device.
        Some(saved) => Plan::Device(saved.id.clone()),
    }
}
/// `getUserMedia` errors that mean the exact device is unavailable, after
/// which the system default is tried. Busy and denied errors are not.
pub fn device_unavailable(error_name: &str) -> bool {
    matches!(
        error_name,
        "NotFoundError" | "DevicesNotFoundError" | "OverconstrainedError"
    )
}

/// Announces a missing saved camera once, until that camera works again or
/// reappears in the list (so a later disconnect is announced again).
#[derive(Debug, Clone, Default)]
pub struct MissingNotice {
    announced: Option<String>,
}
impl MissingNotice {
    /// Whether this fallback for `id` should be shown.
    pub fn missing(&mut self, id: &str) -> bool {
        if self.announced.as_deref() == Some(id) {
            return false;
        }
        self.announced = Some(id.to_owned());
        true
    }
    /// The saved camera opened or is listed again. Returns whether an
    /// announcement for it was outstanding (and can be withdrawn).
    pub fn restored(&mut self, id: &str) -> bool {
        if self.announced.as_deref() == Some(id) {
            self.announced = None;
            true
        } else {
            false
        }
    }
    /// The preference changed: a new choice starts without a notice.
    pub fn reset(&mut self) {
        self.announced = None;
    }
}

/// One entry of the camera selector besides "System default".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: String,
    /// One-based camera number for unnamed devices (0 for the saved entry).
    pub index: usize,
    /// The saved camera is not connected right now.
    pub missing: bool,
}
/// The listed cameras, plus the saved camera when it is not among them, so
/// the selector still shows the preference instead of silently resetting.
pub fn choices(list: &CameraList, saved: Option<&SavedCamera>) -> Vec<Choice> {
    let mut choices: Vec<Choice> = list
        .devices
        .iter()
        .map(|d| Choice {
            id: d.id.clone(),
            label: d.label.clone(),
            index: d.index,
            missing: false,
        })
        .collect();
    if let Some(saved) = saved.filter(|s| list.find(&s.id).is_none()) {
        choices.push(Choice {
            id: saved.id.clone(),
            label: saved.label.clone(),
            index: 0,
            missing: list.labeled,
        });
    }
    choices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labeled() -> CameraList {
        CameraList::new([("front", "FaceTime HD"), ("usb", "  USB Webcam ")])
    }

    #[test]
    fn saved_preference_round_trips_and_rejects_invalid_values() {
        let saved = SavedCamera::new("usb", "USB Webcam").unwrap();
        assert_eq!(SavedCamera::decode(&saved.encode()), Some(saved));
        assert_eq!(
            SavedCamera::decode(r#"{"id":"usb"}"#).unwrap().label,
            "",
            "older values without a label"
        );
        for text in [
            "",
            "usb",
            "null",
            r#"{"id":""}"#,
            r#"{"id":"   "}"#,
            r#"{"label":"x"}"#,
        ] {
            assert_eq!(SavedCamera::decode(text), None, "{text}");
        }
        let long = "x".repeat(MAX_DEVICE_ID + 1);
        assert_eq!(SavedCamera::new(&long, ""), None);
        let label = "é".repeat(MAX_LABEL);
        let clipped = SavedCamera::new("id", &label).unwrap().label;
        assert!(clipped.len() <= MAX_LABEL && clipped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn list_is_bounded_deduplicated_and_tracks_permission() {
        let list = labeled();
        assert!(list.labeled && !list.needs_permission());
        assert_eq!(list.devices[1].label, "USB Webcam");
        assert_eq!(list.devices[1].index, 2);
        // Before permission Chrome hides IDs and names.
        let hidden = CameraList::new([("", ""), ("", "")]);
        assert!(hidden.devices.is_empty() && hidden.needs_permission());
        assert_eq!(hidden.present, 2);
        assert!(!CameraList::default().needs_permission());
        let many: Vec<(String, String)> = (0..40)
            .map(|i| (format!("id{}", i % 30), String::new()))
            .chain([("x".repeat(MAX_DEVICE_ID + 1), String::new())])
            .collect();
        let many = CameraList::new(many);
        assert_eq!(many.devices.len(), MAX_CAMERAS);
        assert_eq!(many.present, 41);
        assert!(
            many.devices
                .iter()
                .enumerate()
                .all(|(i, d)| d.index == i + 1)
        );
    }

    #[test]
    fn plan_uses_saved_device_and_falls_back_only_when_known_missing() {
        let usb = SavedCamera::new("usb", "USB Webcam").unwrap();
        let gone = SavedCamera::new("gone", "Old cam").unwrap();
        assert_eq!(plan(None, &labeled()), Plan::Default);
        assert_eq!(plan(Some(&usb), &labeled()), Plan::Device("usb".into()));
        assert_eq!(plan(Some(&gone), &labeled()), Plan::Missing);
        // Unlabeled (pre-permission) lists cannot prove absence.
        let hidden = CameraList::new([("", "")]);
        assert_eq!(plan(Some(&gone), &hidden), Plan::Device("gone".into()));
        let temporary = CameraList::new([("temporary", "")]);
        assert_eq!(plan(Some(&gone), &temporary), Plan::Device("gone".into()));
        assert_eq!(
            plan(Some(&gone), &CameraList::default()),
            Plan::Device("gone".into())
        );
        for name in [
            "NotFoundError",
            "OverconstrainedError",
            "DevicesNotFoundError",
        ] {
            assert!(device_unavailable(name));
        }
        for name in ["NotAllowedError", "NotReadableError", "AbortError", ""] {
            assert!(!device_unavailable(name), "{name}");
        }
    }

    #[test]
    fn missing_notice_is_shown_once_until_the_camera_returns() {
        let mut notice = MissingNotice::default();
        assert!(notice.missing("usb"));
        assert!(!notice.missing("usb"), "repeated fallback is quiet");
        assert!(!notice.restored("other"));
        assert!(notice.restored("usb"));
        assert!(!notice.restored("usb"));
        assert!(notice.missing("usb"), "a later disconnect is announced");
        notice.reset();
        assert!(notice.missing("usb"));
        assert!(notice.missing("other"));
    }

    #[test]
    fn choices_keep_a_disconnected_preference_visible() {
        let gone = SavedCamera::new("gone", "Old cam").unwrap();
        let usb = SavedCamera::new("usb", "USB Webcam").unwrap();
        assert_eq!(choices(&labeled(), Some(&usb)).len(), 2);
        let listed = choices(&labeled(), Some(&gone));
        assert_eq!(listed.len(), 3);
        assert!(listed[2].missing && listed[2].label == "Old cam");
        assert!(!listed[0].missing && !listed[1].missing);
        // Without permission the preference is shown but not called missing.
        let hidden = choices(&CameraList::new([("", "")]), Some(&gone));
        assert_eq!(hidden.len(), 1);
        assert!(!hidden[0].missing);
        assert!(choices(&CameraList::default(), None).is_empty());
    }
}
