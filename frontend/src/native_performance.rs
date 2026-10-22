//! A single fixed-size snapshot, independent of the audio worker and log setting.
use std::{sync::Mutex, time::Instant};
use thiscord_shared::performance::{UiPerformance, UiPerformanceReport};

static LAST: Mutex<Option<(Instant, UiPerformance)>> = Mutex::new(None);

#[tauri::command]
pub fn performance_report(
    window: tauri::WebviewWindow,
    measurements: UiPerformance,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Main window required".into());
    }
    *LAST
        .lock()
        .map_err(|_| "Performance counters unavailable")? = Some((Instant::now(), measurements));
    Ok(())
}

pub fn snapshot() -> Option<UiPerformanceReport> {
    let last = LAST.lock().ok()?;
    let (at, measurements) = last.as_ref()?;
    Some(UiPerformanceReport {
        age_ms: at.elapsed().as_millis().min(u64::MAX as u128) as u64,
        measurements: measurements.clone(),
    })
}
