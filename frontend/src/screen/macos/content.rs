//! Shareable displays/windows, Screen Recording permission and window state.
use super::Sendable;
use block2::RcBlock;
use objc2::{AnyThread, rc::Retained};
use objc2_core_graphics::{
    CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayPixelsWide, CGMainDisplayID,
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCContentFilter, SCDisplay, SCShareableContent, SCWindow};
use std::time::Duration;
use thiscord_shared::screen::{Source, SourceId};

const PERMISSION: &str = "Allow Thiscord under System Settings › Privacy & Security › \
     Screen & System Audio Recording, then restart Thiscord.";
/// SCStreamErrorUserDeclined.
const USER_DECLINED: isize = -3801;

pub(crate) fn describe_error(error: &NSError) -> String {
    if error.code() == USER_DECLINED {
        return format!("Screen Recording permission is required. {PERMISSION}");
    }
    format!("{} ({})", error.localizedDescription(), error.code())
}
/// Adds the permission instructions to opaque SCK start failures.
pub(crate) fn permission_hint(error: String) -> String {
    if error.contains("Screen Recording") || CGPreflightScreenCaptureAccess() {
        error
    } else {
        format!("{error}. {PERMISSION}")
    }
}

/// Preflight never prompts; requesting once registers Thiscord in the list.
pub(crate) fn require_permission() -> Result<(), String> {
    if CGPreflightScreenCaptureAccess() {
        return Ok(());
    }
    // Shows the system prompt (first time) and lists the app in Settings.
    CGRequestScreenCaptureAccess();
    Err(format!(
        "Screen Recording permission is required. {PERMISSION}"
    ))
}

pub(crate) fn shareable() -> Result<Retained<SCShareableContent>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: SCK passes a valid object or null for each argument.
            let result = match unsafe { (Retained::retain(content), error.as_ref()) } {
                (Some(content), _) => Ok(Sendable(content)),
                (None, Some(error)) => Err(describe_error(error)),
                (None, None) => Err("No shareable content".to_owned()),
            };
            let _ = tx.try_send(result);
        },
    );
    // SAFETY: the block lives until the call returns; SCK copies it.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &block,
        )
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(content)) => Ok(content.0),
        Ok(Err(error)) => Err(format!("Cannot list screens and windows: {error}")),
        Err(_) => Err("Cannot list screens and windows: timed out".into()),
    }
}

fn display_pixels(display: &SCDisplay) -> (u32, u32) {
    // SAFETY: plain property reads / CoreGraphics queries by display ID.
    let id = unsafe { display.displayID() };
    let mode = CGDisplayCopyDisplayMode(id);
    let (w, h) = (
        CGDisplayMode::pixel_width(mode.as_deref()),
        CGDisplayMode::pixel_height(mode.as_deref()),
    );
    if w >= 2 && h >= 2 {
        (w as u32, h as u32)
    } else {
        // SAFETY: plain property reads.
        unsafe { (display.width() as u32, display.height() as u32) }
    }
}
/// Pixels per point: the filter's own scale (macOS 14+) or the main display's.
fn point_scale(filter: &SCContentFilter) -> f64 {
    if super::at_least(14, 0) {
        // SAFETY: available on macOS 14+, checked above.
        let scale = unsafe { filter.pointPixelScale() };
        if scale.is_finite() && scale >= 1.0 {
            return f64::from(scale);
        }
    }
    let main = CGMainDisplayID();
    let mode = CGDisplayCopyDisplayMode(main);
    let points = CGDisplayPixelsWide(main) as f64;
    let pixels = CGDisplayMode::pixel_width(mode.as_deref()) as f64;
    if points >= 1.0 && pixels >= points {
        pixels / points
    } else {
        1.0
    }
}

fn own_window(window: &SCWindow) -> bool {
    // SAFETY: plain property reads.
    unsafe { window.owningApplication() }
        .is_some_and(|app| unsafe { app.processID() } == std::process::id() as i32)
}
fn shareable_window(window: &SCWindow) -> Option<String> {
    // SAFETY: plain property reads on an immutable snapshot.
    unsafe {
        let frame = window.frame();
        if window.windowLayer() != 0
            || !window.isOnScreen()
            || frame.size.width < 40.0
            || frame.size.height < 40.0
            || own_window(window)
        {
            return None;
        }
        let title = window.title()?.to_string();
        if title.trim().is_empty() {
            return None;
        }
        let app = window
            .owningApplication()
            .map(|app| app.applicationName().to_string())
            .unwrap_or_default();
        Some(if app.is_empty() || app == title {
            title
        } else {
            format!("{app} — {title}")
        })
    }
}

pub fn sources() -> Result<Vec<Source>, String> {
    super::require_capture()?;
    let content = shareable()?;
    let mut sources = Vec::new();
    // SAFETY: immutable snapshot arrays.
    let displays = unsafe { content.displays() };
    let count = displays.len();
    for (index, display) in displays.iter().enumerate() {
        let (w, h) = display_pixels(&display);
        // SAFETY: plain property read.
        let id = unsafe { display.displayID() };
        sources.push(Source {
            id: SourceId::Monitor(u64::from(id)),
            label: if count == 1 {
                format!("Screen: Display ({w}×{h})")
            } else {
                format!("Screen: Display {} ({w}×{h})", index + 1)
            },
        });
    }
    // SAFETY: immutable snapshot array.
    for window in unsafe { content.windows() }.iter() {
        if let Some(label) = shareable_window(&window) {
            sources.push(Source {
                // SAFETY: plain property read.
                id: SourceId::Window(u64::from(unsafe { window.windowID() })),
                label,
            });
        }
    }
    sources.truncate(256);
    Ok(sources)
}

/// The selected content resolved against a fresh snapshot.
pub(crate) struct Target {
    pub filter: Retained<SCContentFilter>,
    pub pixels: (u32, u32),
    pub window: Option<u32>,
    /// Pixels per point of the shared window, for resize tracking.
    pub scale: f64,
}
impl Target {
    pub fn resolve(source: &SourceId) -> Result<Self, String> {
        let content = shareable()?;
        match *source {
            SourceId::Monitor(id) => {
                // SAFETY: immutable snapshot array.
                let display = unsafe { content.displays() }
                    .iter()
                    .find(|d| u64::from(unsafe { d.displayID() }) == id)
                    .ok_or("Selected screen is no longer available")?;
                Ok(Self {
                    filter: super::display_filter(&display),
                    pixels: display_pixels(&display),
                    window: None,
                    scale: 1.0,
                })
            }
            SourceId::Window(id) => {
                // SAFETY: immutable snapshot array.
                let window = unsafe { content.windows() }
                    .iter()
                    .find(|w| {
                        u64::from(unsafe { w.windowID() }) == id && shareable_window(w).is_some()
                    })
                    .ok_or("Selected window is no longer available")?;
                // SAFETY: a valid on-screen window from this snapshot.
                let filter = unsafe {
                    SCContentFilter::initWithDesktopIndependentWindow(
                        SCContentFilter::alloc(),
                        &window,
                    )
                };
                let scale = point_scale(&filter);
                // SAFETY: plain property read.
                let frame = unsafe { window.frame() };
                let pixels = (
                    ((frame.size.width * scale).round() as u32).max(2),
                    ((frame.size.height * scale).round() as u32).max(2),
                );
                Ok(Self {
                    filter,
                    pixels,
                    // SAFETY: plain property read.
                    window: Some(unsafe { window.windowID() }),
                    scale,
                })
            }
        }
    }
}

/// False once the window no longer exists. A minimized window or one on
/// another Space keeps its stream (SCK holds the last image), so switching
/// Spaces does not end the share; closing the window does.
pub(crate) fn window_exists(id: u32) -> bool {
    CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, id)
        .is_some_and(|list| list.count() > 0)
}

/// Current window size in points; `None` once the window no longer exists.
pub(crate) fn window_points(id: u32) -> Option<(f64, f64)> {
    use objc2_core_foundation::{CFDictionary, CGRect};
    let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, id)?;
    if list.count() < 1 {
        return None;
    }
    // SAFETY: index 0 is in bounds; elements are CFDictionaryRef owned by the list.
    let info = unsafe { list.value_at_index(0) }.cast::<CFDictionary>();
    // SAFETY: kCGWindowBounds is a constant CFString key.
    let key: *const objc2_core_foundation::CFString =
        unsafe { objc2_core_graphics::kCGWindowBounds };
    // SAFETY: non-null checked; the value is a rect dictionary or null.
    let bounds = unsafe { info.as_ref()?.value(key.cast()) }.cast::<CFDictionary>();
    let mut rect = CGRect::default();
    // SAFETY: `bounds` is null or a rect dictionary owned by `list`; `rect` is valid.
    let parsed = unsafe {
        objc2_core_graphics::CGRectMakeWithDictionaryRepresentation(bounds.as_ref(), &mut rect)
    };
    parsed.then_some((rect.size.width, rect.size.height))
}
