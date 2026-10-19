//! Presentation only: no credentials or network access in the banner WebView.
use std::sync::{Mutex, OnceLock, mpsc};
use tauri::{Emitter, Manager};
use thiscord_shared::notifications::*;

#[derive(Default)]
pub struct NotificationState(Mutex<Presentation>);
#[derive(Default)]
struct Presentation {
    generation: u32,
    revision: u64,
    alerts: Vec<Alert>,
    ringing: Vec<thiscord_shared::ids::CallId>,
}
impl Presentation {
    fn clear(&mut self) -> u32 {
        self.revision += 1;
        self.generation = self.generation.wrapping_add(1);
        self.alerts.clear();
        self.ringing.clear();
        self.generation
    }
    fn sync_calls(&mut self, ringing: Vec<thiscord_shared::ids::CallId>) {
        self.revision += 1;
        self.alerts
            .retain(|a| a.call_id.is_none_or(|id| ringing.contains(&id)));
        self.ringing = ringing;
    }
    fn insert(&mut self, generation: u32, alert: Alert) -> bool {
        // Window creation is asynchronous. A call can end before it completes.
        if generation != self.generation
            || alert.call_id.is_some_and(|id| !self.ringing.contains(&id))
        {
            return false;
        }
        self.alerts.retain(|a| a.id != alert.id);
        self.revision += 1;
        self.alerts.push(alert);
        if self.alerts.len() > 3 {
            self.alerts.remove(0);
        }
        true
    }
    fn snapshot(&self) -> BannerSnapshot {
        BannerSnapshot {
            revision: self.revision,
            alerts: self.alerts.clone(),
        }
    }
    fn remove(&mut self, id: &str) {
        self.revision += 1;
        self.alerts.retain(|a| a.id != id);
    }
    fn layout_height(&self, revision: u64, height: f64) -> Option<f64> {
        (self.revision == revision && !self.alerts.is_empty() && height.is_finite())
            .then(|| height.clamp(80.0, 640.0))
    }
}
fn update_banner_on_main(
    app: &tauri::AppHandle,
    update: impl FnOnce(&tauri::AppHandle) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    // Window getters wait for the UI thread. Dispatch before taking the state
    // mutex, or a synchronous IPC handler waiting for that mutex can deadlock.
    let handle = app.clone();
    app.run_on_main_thread(move || {
        if let Err(error) = update(&handle) {
            eprintln!("Notification window update failed: {error}");
        }
    })
    .map_err(|_| "Cannot schedule notification window update".into())
}
fn publish(app: &tauri::AppHandle) -> Result<(), String> {
    update_banner_on_main(app, |app| {
        // Read current state only after dispatch, serialized with clearing:
        // queued updates must never show a pre-logout snapshot.
        let state = app.state::<NotificationState>();
        let state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        app.emit_to("notifications", "notification-banners", state.snapshot())
            .map_err(|_| "Cannot update notification banner")?;
        if let Some(window) = app.get_webview_window("notifications") {
            if state.alerts.is_empty() {
                let _ = window.hide();
            } else if !window.is_visible().unwrap_or(false) {
                // Hidden WebViews may suspend animation frames/ResizeObserver.
                // Show a compact window first so content can be measured.
                fit_banner(app, &window, 128.0)?;
                window
                    .show()
                    .map_err(|_| "Cannot show notification banner")?;
            }
        }
        Ok(())
    })
}
/// Only the banner WebView can size its own window, after rendering the current
/// snapshot. Never let a delayed measurement reopen cleared/expired banners.
#[tauri::command]
pub fn notification_resize(
    app: tauri::AppHandle,
    revision: u64,
    height: f64,
) -> Result<(), String> {
    update_banner_on_main(&app, move |app| {
        let state = app.state::<NotificationState>();
        let state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        let Some(height) = state.layout_height(revision, height) else {
            return Ok(());
        };
        let Some(window) = app.get_webview_window("notifications") else {
            return Ok(());
        };
        fit_banner(app, &window, height)
    })
}
fn fit_banner(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    mut height: f64,
) -> Result<(), String> {
    let monitor = app
        .get_webview_window("main")
        .and_then(|main| main.current_monitor().ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten());
    let mut width: f64 = 396.0;
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        width = width.min((f64::from(area.size.width) / scale - 32.0).max(1.0));
        height = height.min((f64::from(area.size.height) / scale - 32.0).max(1.0));
        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|_| "Cannot size notification banner")?;
        window
            .set_position(tauri::PhysicalPosition::new(
                area.position.x + area.size.width as i32 - ((width + 16.0) * scale).round() as i32,
                area.position.y + area.size.height as i32
                    - ((height + 16.0) * scale).round() as i32,
            ))
            .map_err(|_| "Cannot position notification banner")?;
    } else {
        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|_| "Cannot size notification banner")?;
    }
    Ok(())
}
#[tauri::command]
pub fn notification_snapshot(
    state: tauri::State<'_, NotificationState>,
) -> Result<BannerSnapshot, String> {
    state
        .0
        .lock()
        .map(|v| v.snapshot())
        .map_err(|_| "Notifications unavailable".into())
}
#[tauri::command]
pub fn notification_clear(app: tauri::AppHandle) -> Result<u32, String> {
    let generation = app
        .state::<NotificationState>()
        .0
        .lock()
        .map_err(|_| "Notifications unavailable")?
        .clear();
    publish(&app)?;
    Ok(generation)
}
#[tauri::command]
pub fn notification_remove(
    app: tauri::AppHandle,
    generation: u32,
    id: String,
) -> Result<(), String> {
    {
        let state = app.state::<NotificationState>();
        let mut state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        if state.generation != generation {
            return Ok(());
        }
        state.remove(&id);
    }
    publish(&app)
}
#[tauri::command]
pub fn notification_sync_calls(
    app: tauri::AppHandle,
    generation: u32,
    ringing: Vec<thiscord_shared::ids::CallId>,
) -> Result<(), String> {
    {
        let state = app.state::<NotificationState>();
        let mut state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        if state.generation != generation {
            return Ok(());
        }
        state.sync_calls(ringing);
    }
    publish(&app)
}
#[tauri::command]
pub fn notification_snoozed(
    app: tauri::AppHandle,
    generation: u32,
    target: Target,
) -> Result<(), String> {
    {
        let state = app.state::<NotificationState>();
        let mut state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        if state.generation != generation {
            return Ok(());
        }
        state.alerts.retain(|a| a.target != target);
        state.revision += 1;
    }
    publish(&app)
}
#[tauri::command]
pub async fn notification_show(
    app: tauri::AppHandle,
    generation: u32,
    mut alert: Alert,
) -> Result<(), String> {
    alert.title = alert.title.chars().take(160).collect();
    if app.get_webview_window("notifications").is_none() {
        tauri::WebviewWindowBuilder::new(
            &app,
            "notifications",
            tauri::WebviewUrl::App("index.html#notifications".into()),
        )
        .title("Thiscord notifications")
        .inner_size(396.0, 128.0)
        .transparent(true)
        .shadow(false)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|_| "Desktop banners unavailable")?;
    }
    let id = alert.id.clone();
    let seconds = if alert.kind == Kind::IncomingCall {
        30
    } else {
        12
    };
    {
        let state = app.state::<NotificationState>();
        let mut state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        if !state.insert(generation, alert) {
            return Ok(());
        }
    }
    publish(&app)?;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        let _ = notification_remove(app, generation, id);
    });
    Ok(())
}
#[tauri::command]
pub fn notification_action(
    app: tauri::AppHandle,
    id: String,
    action: BannerAction,
) -> Result<(), String> {
    let (alert, generation) = {
        let state = app.state::<NotificationState>();
        let state = state.0.lock().map_err(|_| "Notifications unavailable")?;
        (
            state
                .alerts
                .iter()
                .find(|a| a.id == id)
                .cloned()
                .ok_or("Notification expired")?,
            state.generation,
        )
    };
    if matches!(action, BannerAction::Open | BannerAction::Accept)
        && let Some(main) = app.get_webview_window("main")
    {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
    app.emit_to(
        "main",
        "notification-action",
        BannerInteraction {
            generation,
            alert,
            action: action.clone(),
        },
    )
    .map_err(|_| "Cannot open notification")?;
    if matches!(action, BannerAction::Snooze { .. }) {
        Ok(())
    } else {
        notification_remove(app, generation, id)
    }
}
#[tauri::command]
pub async fn notification_sound(
    app: tauri::AppHandle,
    kind: Kind,
    volume: u8,
) -> Result<(), String> {
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    let status = tauri::async_runtime::spawn_blocking(move || {
        engine.command(thiscord_frontend::audio::Command::Notification { kind, volume })
    })
    .await
    .map_err(|_| "Audio worker unavailable")??;
    if status.running {
        return Ok(());
    }
    static SOUNDS: OnceLock<mpsc::SyncSender<(Kind, u8)>> = OnceLock::new();
    let sender = SOUNDS.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            while let Ok((kind, volume)) = receiver.recv() {
                let _ = play(kind, volume);
            }
        });
        sender
    });
    // Coalesce bursts by dropping sounds when the bounded playback queue is full.
    let _ = sender.try_send((kind, volume.min(100)));
    Ok(())
}
fn playback_drain_delay(
    timestamp: cpal::OutputStreamTimestamp,
    frames: usize,
    rate: u32,
) -> std::time::Duration {
    timestamp.playback.duration_since(timestamp.callback)
        + std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate))
        + std::time::Duration::from_millis(20)
}
fn play(kind: Kind, volume: u8) -> Result<(), ()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device().ok_or(())?;
    let supported = device.default_output_config().map_err(|_| ())?;
    let config: cpal::StreamConfig = supported.into();
    let channels = usize::from(config.channels);
    let mut samples = thiscord_frontend::notification_sound::render(kind, config.sample_rate);
    for sample in &mut samples {
        *sample *= f32::from(volume.min(100)) / 100.0;
    }
    // Publish the predicted DAC deadline of the final sample, rather than
    // treating submission to a device buffer as completion of playback.
    let finished_at = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let started = std::time::Instant::now();
    fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Vec<f32>,
        channels: usize,
        finished_at: std::sync::Arc<std::sync::atomic::AtomicU64>,
        started: std::time::Instant,
    ) -> Result<cpal::Stream, ()> {
        use cpal::traits::DeviceTrait;
        let mut position = 0;
        let rate = config.sample_rate;
        device
            .build_output_stream(
                *config,
                move |output: &mut [T], info: &cpal::OutputCallbackInfo| {
                    let remaining = samples.len().saturating_sub(position);
                    for frame in output.chunks_mut(channels) {
                        let value = T::from_sample(samples.get(position).copied().unwrap_or(0.0));
                        frame.fill(value);
                        position = position.saturating_add(1);
                    }
                    if remaining > 0 && position >= samples.len() {
                        let deadline = started.elapsed()
                            + playback_drain_delay(info.timestamp(), remaining, rate);
                        finished_at.store(
                            deadline.as_micros() as u64,
                            std::sync::atomic::Ordering::Release,
                        );
                    }
                },
                |_| {},
                None,
            )
            .map_err(|_| ())
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => stream::<f32>(
            &device,
            &config,
            samples,
            channels,
            finished_at.clone(),
            started,
        )?,
        cpal::SampleFormat::I16 => stream::<i16>(
            &device,
            &config,
            samples,
            channels,
            finished_at.clone(),
            started,
        )?,
        cpal::SampleFormat::U16 => stream::<u16>(
            &device,
            &config,
            samples,
            channels,
            finished_at.clone(),
            started,
        )?,
        _ => return Err(()),
    };
    stream.play().map_err(|_| ())?;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(
            thiscord_frontend::notification_sound::duration_ms(kind) as u64 + 2000,
        );
    loop {
        let finished_at = finished_at.load(std::sync::atomic::Ordering::Acquire);
        if finished_at != 0 && started.elapsed().as_micros() >= u128::from(finished_at) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_drain_waits_for_device_latency_and_large_final_buffers() {
        use std::time::Duration;
        let callback = cpal::StreamInstant::from_secs_f64(1.0);
        let timestamp = cpal::OutputStreamTimestamp {
            callback,
            playback: callback.checked_add(Duration::from_millis(200)).unwrap(),
        };
        let delay = playback_drain_delay(timestamp, 8192, 48000);
        assert!(delay >= Duration::from_millis(390));
        assert!(delay < Duration::from_millis(392));
        // Only 480 audible frames remain in this buffer: drain to their end,
        // not to the end of the full 8192-frame allocation.
        assert_eq!(
            playback_drain_delay(timestamp, 480, 48000),
            Duration::from_millis(230)
        );
    }
    #[test]
    fn logout_rejects_delayed_message_from_the_previous_session() {
        let alert = Alert {
            id: "old-message".into(),
            target: Target::Updates,
            kind: Kind::Update,
            title: "Old account notification".into(),
            delivery: Delivery::default_for(Kind::Update),
            actor: None,
            call_id: None,
            version: None,
        };
        let mut state = Presentation::default();
        let old_generation = state.generation;
        assert!(state.insert(old_generation, alert.clone()));
        let old_revision = state.revision;
        assert_eq!(state.layout_height(old_revision, 122.0), Some(122.0));
        assert_eq!(state.layout_height(old_revision, 10000.0), Some(640.0));
        assert_eq!(state.layout_height(old_revision, f64::NAN), None);
        let new_generation = state.clear();
        assert_eq!(state.layout_height(old_revision, 122.0), None);
        assert_eq!(state.layout_height(state.revision, 122.0), None);
        assert!(!state.insert(old_generation, alert.clone()));
        assert!(state.alerts.is_empty());
        assert!(state.insert(new_generation, alert));
        assert_eq!(state.layout_height(old_revision, 122.0), None);
        assert_eq!(state.layout_height(state.revision, 102.0), Some(102.0));
        state.remove("old-message");
        assert_eq!(state.layout_height(state.revision, 102.0), None);
    }
    #[test]
    fn ending_a_call_removes_its_banner_and_rejects_delayed_presentation() {
        let call = "11111111-1111-4111-8111-111111111111".parse().unwrap();
        let conversation = "22222222-2222-4222-8222-222222222222".parse().unwrap();
        let alert = Alert {
            id: format!("{call}"),
            target: Target::Conversation {
                conversation_id: conversation,
            },
            kind: Kind::IncomingCall,
            title: "Incoming call".into(),
            delivery: Delivery::default_for(Kind::IncomingCall),
            actor: None,
            call_id: Some(call),
            version: None,
        };
        let mut state = Presentation::default();
        state.sync_calls(vec![call]);
        assert!(state.insert(0, alert.clone()));
        state.sync_calls(vec![]);
        assert!(state.alerts.is_empty());
        assert!(!state.insert(0, alert));
        assert!(state.alerts.is_empty());
    }
}
