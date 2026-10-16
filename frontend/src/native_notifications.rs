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
}
fn publish(app: &tauri::AppHandle) -> Result<(), String> {
    // Serialize presentation with clearing: never emit an old cloned snapshot
    // after a newer empty snapshot from logout.
    let state = app.state::<NotificationState>();
    let state = state.0.lock().map_err(|_| "Notifications unavailable")?;
    app.emit_to("notifications", "notification-banners", state.snapshot())
        .map_err(|_| "Cannot update notification banner")?;
    if let Some(window) = app.get_webview_window("notifications") {
        if state.alerts.is_empty() {
            let _ = window.hide();
        } else {
            let _ = window.show();
        }
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
        let window = tauri::WebviewWindowBuilder::new(
            &app,
            "notifications",
            tauri::WebviewUrl::App("index.html#notifications".into()),
        )
        .title("Thiscord notifications")
        .inner_size(380.0, 360.0)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|_| "Desktop banners unavailable")?;
        if let Some(main) = app.get_webview_window("main")
            && let Ok(Some(monitor)) = main.current_monitor()
        {
            let scale = monitor.scale_factor();
            let origin = monitor.position();
            let size = monitor.size();
            let _ = window.set_position(tauri::PhysicalPosition::new(
                origin.x + size.width as i32 - (400.0 * scale) as i32,
                origin.y + size.height as i32 - (420.0 * scale) as i32,
            ));
        }
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
fn play(kind: Kind, volume: u8) -> Result<(), ()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device().ok_or(())?;
    let supported = device.default_output_config().map_err(|_| ())?;
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate as usize;
    let channels = usize::from(config.channels);
    let frequency = match kind {
        Kind::VoiceJoin => 880.0,
        Kind::VoiceLeave => 440.0,
        Kind::IncomingCall => 740.0,
        Kind::Update => 520.0,
        _ => 660.0,
    };
    let length = rate / 4;
    let samples: Vec<f32> = (0..length)
        .map(|n| {
            let envelope = (std::f32::consts::PI * n as f32 / length as f32)
                .sin()
                .powi(2);
            0.16 * f32::from(volume) / 100.0
                * envelope
                * (std::f32::consts::TAU * frequency * n as f32 / rate as f32).sin()
        })
        .collect();
    fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Vec<f32>,
        channels: usize,
    ) -> Result<cpal::Stream, ()> {
        use cpal::traits::DeviceTrait;
        let mut position = 0;
        device
            .build_output_stream(
                *config,
                move |output: &mut [T], _| {
                    for frame in output.chunks_mut(channels) {
                        let value = T::from_sample(samples.get(position).copied().unwrap_or(0.0));
                        frame.fill(value);
                        position = position.saturating_add(1);
                    }
                },
                |_| {},
                None,
            )
            .map_err(|_| ())
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => stream::<f32>(&device, &config, samples, channels)?,
        cpal::SampleFormat::I16 => stream::<i16>(&device, &config, samples, channels)?,
        cpal::SampleFormat::U16 => stream::<u16>(&device, &config, samples, channels)?,
        _ => return Err(()),
    };
    stream.play().map_err(|_| ())?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let new_generation = state.clear();
        assert!(!state.insert(old_generation, alert.clone()));
        assert!(state.alerts.is_empty());
        assert!(state.insert(new_generation, alert));
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
