//! Webcam enumeration, the device-local camera preference and its missing-device
//! fallback, shared by the desktop WebView camera (`app/camera.rs`) and the
//! `/app` browser client (`browser_voice/video.rs`). Both capture in the page,
//! so `enumerateDevices`/`getUserMedia` device IDs are used directly; no native
//! camera API is involved. Rules live in `thiscord_frontend::camera_choice`.
use leptos::prelude::*;
use std::cell::{Cell, RefCell};
use thiscord_frontend::{
    browser_video::{self, Facing},
    camera_choice::{self as rules, CameraList, MissingNotice, Plan, SavedCamera},
};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    MediaDeviceInfo, MediaDeviceKind, MediaDevices, MediaStream, MediaStreamConstraints,
    MediaStreamTrack,
};

thread_local! {
    static LIST: ArcRwSignal<CameraList> = ArcRwSignal::new(CameraList::default());
    static SAVED: ArcRwSignal<Option<SavedCamera>> = ArcRwSignal::new(load());
    static NOTICE: ArcRwSignal<String> = ArcRwSignal::new(String::new());
    static MISSING: RefCell<MissingNotice> = RefCell::new(MissingNotice::default());
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    /// The first enumeration finished (avoids a "no camera" flash).
    static LISTED: ArcRwSignal<bool> = ArcRwSignal::new(false);
    /// This client publishes a camera in the current call.
    static CALL_CAMERA: ArcRwSignal<bool> = ArcRwSignal::new(false);
    /// Increments on every selection, so a capture can tell whether it
    /// opened an outdated choice.
    static REVISION: ArcRwSignal<u32> = ArcRwSignal::new(0);
}

fn media_devices() -> Option<MediaDevices> {
    web_sys::window()?.navigator().media_devices().ok()
}
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
fn load() -> Option<SavedCamera> {
    storage()?
        .get_item(rules::STORAGE_KEY)
        .ok()
        .flatten()
        .and_then(|text| SavedCamera::decode(&text))
}
fn error_name(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &"name".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}
pub fn stop_stream(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        track.unchecked_into::<MediaStreamTrack>().stop();
    }
}

/// Listed cameras (reactive).
pub fn list() -> CameraList {
    LIST.with(|list| list.get())
}
/// Whether cameras were enumerated at least once (reactive).
pub fn listed() -> bool {
    LISTED.with(|listed| listed.get())
}
/// Whether the call camera is on (reactive), for the settings preview.
pub fn call_camera() -> bool {
    CALL_CAMERA.with(|active| active.get())
}
pub fn set_call_camera(active: bool) {
    CALL_CAMERA.with(|current| {
        if current.get_untracked() != active {
            current.set(active);
        }
    });
}
/// The saved camera, if any (reactive).
pub fn saved() -> Option<SavedCamera> {
    SAVED.with(|saved| saved.get())
}
pub fn saved_untracked() -> Option<SavedCamera> {
    SAVED.with(|saved| saved.get_untracked())
}
/// Selection counter (reactive): changes whenever the user picks a camera.
pub fn revision() -> u32 {
    REVISION.with(|revision| revision.get())
}
pub fn revision_untracked() -> u32 {
    REVISION.with(|revision| revision.get_untracked())
}
/// The one-time "saved camera missing" notice (reactive).
pub fn notice() -> String {
    NOTICE.with(|notice| notice.get())
}
pub fn dismiss_notice() {
    NOTICE.with(|notice| notice.set(String::new()));
}
/// Display name of a listed or saved camera.
pub fn label(label: &str, index: usize) -> String {
    if !label.is_empty() {
        label.to_owned()
    } else if index > 0 {
        thiscord_frontend::localized_format!(crate::i18n::language(), "Camera {}", index)
    } else {
        crate::i18n::t("Saved camera").to_owned()
    }
}

/// Remembers a camera on this device, or the system default with `None`.
/// Storage failures keep the choice for this window only.
pub fn select(id: Option<String>) {
    let saved = id.and_then(|id| {
        let list = LIST.with(|list| list.get_untracked());
        let label = list
            .find(&id)
            .map(|d| d.label.clone())
            .or_else(|| saved_untracked().filter(|s| s.id == id).map(|s| s.label))
            .unwrap_or_default();
        SavedCamera::new(&id, &label)
    });
    if saved == saved_untracked() {
        return;
    }
    if let Some(storage) = storage() {
        let _ = match &saved {
            Some(saved) => storage.set_item(rules::STORAGE_KEY, &saved.encode()),
            None => storage.remove_item(rules::STORAGE_KEY),
        };
    }
    MISSING.with(|missing| missing.borrow_mut().reset());
    dismiss_notice();
    SAVED.with(|s| s.set(saved));
    REVISION.with(|revision| revision.update(|r| *r = r.wrapping_add(1)));
}

/// Re-enumerates video inputs. Names (and stable IDs) appear only after the
/// origin was granted camera access.
pub async fn refresh() -> CameraList {
    let list = enumerate().await;
    // A remembered camera that is listed again gets a fresh notice if it
    // disappears later. A shown notice stays: a running camera keeps using
    // the default until it is restarted or switched.
    if list.labeled
        && let Some(saved) = saved_untracked()
        && list.find(&saved.id).is_some()
    {
        MISSING.with(|missing| missing.borrow_mut().restored(&saved.id));
    }
    LIST.with(|current| {
        if current.get_untracked() != list {
            current.set(list.clone());
        }
    });
    LISTED.with(|listed| {
        if !listed.get_untracked() {
            listed.set(true);
        }
    });
    list
}
async fn enumerate() -> CameraList {
    let Some(devices) = media_devices() else {
        return CameraList::default();
    };
    let Ok(promise) = devices.enumerate_devices() else {
        return CameraList::default();
    };
    let Ok(found) = JsFuture::from(promise).await else {
        return CameraList::default();
    };
    CameraList::new(
        js_sys::Array::from(&found)
            .iter()
            .filter_map(|device| device.dyn_into::<MediaDeviceInfo>().ok())
            .filter(|device| device.kind() == MediaDeviceKind::Videoinput)
            .map(|device| (device.device_id(), device.label())),
    )
}

/// Lists cameras now and follows `devicechange` for the rest of this page.
pub fn watch() {
    if WATCHING.with(|watching| watching.replace(true)) {
        return;
    }
    leptos::task::spawn_local(async {
        refresh().await;
    });
    let Some(devices) = media_devices() else {
        return;
    };
    let changed = Closure::<dyn FnMut()>::new(|| {
        leptos::task::spawn_local(async {
            refresh().await;
        });
    });
    if devices
        .add_event_listener_with_callback("devicechange", changed.as_ref().unchecked_ref())
        .is_ok()
    {
        // One listener for the page's lifetime; `WATCHING` prevents repeats.
        changed.forget();
    }
}

/// Opens the camera: the saved one unless `use_saved` is false (front/back
/// flipping on phones), else the system default or `facing`. A saved camera
/// that is missing falls back to the system default with a one-time notice;
/// the preference is kept, so it is used again once reconnected.
pub async fn open(facing: Option<Facing>, use_saved: bool) -> Result<MediaStream, JsValue> {
    let devices = media_devices().ok_or(JsValue::NULL)?;
    let saved = use_saved.then(saved_untracked).flatten();
    let list = if saved.is_some() {
        refresh().await
    } else {
        CameraList::default()
    };
    let plan = rules::plan(saved.as_ref(), &list);
    let request = |device: Option<&str>| -> Result<js_sys::Promise, JsValue> {
        let constraints = MediaStreamConstraints::new();
        constraints.set_audio(&JsValue::FALSE);
        let video = browser_video::camera_constraints(device, facing);
        let video = js_sys::JSON::parse(&video.to_string()).unwrap_or(JsValue::TRUE);
        constraints.set_video(&video);
        devices.get_user_media_with_constraints(&constraints)
    };
    let device = match &plan {
        Plan::Device(id) => Some(id.as_str()),
        Plan::Default | Plan::Missing => None,
    };
    let mut missing = plan == Plan::Missing;
    let mut result = JsFuture::from(request(device)?).await;
    if device.is_some()
        && let Err(error) = &result
        && rules::device_unavailable(&error_name(error))
    {
        missing = true;
        result = JsFuture::from(request(None)?).await;
    }
    let stream: MediaStream = result?.unchecked_into();
    if let Some(saved) = &saved {
        if missing {
            if MISSING.with(|notice| notice.borrow_mut().missing(&saved.id)) {
                let text = if saved.label.is_empty() {
                    crate::i18n::t(
                        "Your saved camera isn’t connected. Using the system default camera.",
                    )
                    .to_owned()
                } else {
                    thiscord_frontend::localized_format!(
                        crate::i18n::language(),
                        "{} isn’t connected. Using the system default camera.",
                        saved.label
                    )
                };
                NOTICE.with(|notice| notice.set(text));
            }
        } else {
            // The notice can only concern the saved camera, which now opened.
            MISSING.with(|notice| notice.borrow_mut().restored(&saved.id));
            dismiss_notice();
        }
    }
    // Names become available once permission was granted.
    leptos::task::spawn_local(async {
        refresh().await;
    });
    Ok(stream)
}

/// Error text for a failed camera request, in the client's wording.
pub fn error_message(error: &JsValue) -> &'static str {
    let desktop = crate::account_client::desktop();
    match error_name(error).as_str() {
        "NotAllowedError" | "PermissionDeniedError" | "SecurityError" if desktop => {
            "Camera access was denied. Allow camera access in your system privacy settings."
        }
        "NotAllowedError" | "PermissionDeniedError" | "SecurityError" => {
            "Camera access is blocked. Allow the camera for this site in your browser settings."
        }
        "NotFoundError" | "DevicesNotFoundError" | "OverconstrainedError" => {
            "No camera found. Connect a webcam or enable a virtual camera."
        }
        "NotReadableError" | "TrackStartError" | "AbortError" => {
            "The camera is busy or unavailable. Close other camera apps and try again."
        }
        _ if desktop => "Cannot start the camera on this WebView.",
        _ => "Cannot start the camera in this browser.",
    }
}
