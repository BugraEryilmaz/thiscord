//! Explicit admin requests arrive only on the authenticated voice signaling connection.
use tauri::Manager;
use thiscord_frontend::audio::{connection::Connection, diagnostics};
use thiscord_shared::remote_audio::{Command, Reply};

pub async fn execute(app: tauri::AppHandle, connection: Connection, command: Command) -> Reply {
    use crate::native_audio as audio;
    command.validate().map_err(str::to_owned)?;
    if !connection.active() {
        return Err("Voice connection ended".into());
    }
    let value = match command {
        Command::AudioCurrent {} => serde_json::to_value(audio::audio_current(app).await?),
        Command::AudioDevices {} => serde_json::to_value(audio::audio_devices().await?),
        Command::AudioStatus {} => {
            let mut status = audio::audio_status(app.clone(), app.state()).await?;
            status.recording = None;
            serde_json::to_value(status)
        }
        Command::AudioSave { settings, expected } => {
            audio::remote_save(app, settings, expected, connection).await?;
            Ok(serde_json::Value::Null)
        }
        Command::AudioToggle { action } => {
            audio::audio_toggle(app, action).await?;
            Ok(serde_json::Value::Null)
        }
        Command::AudioVolume {
            stream,
            gain,
            target,
        } => {
            let mut status = audio::audio_volume(app, stream, gain, target).await?;
            status.recording = None;
            serde_json::to_value(status)
        }
        Command::AudioShortcuts {} => {
            serde_json::to_value(crate::native_shortcuts::audio_shortcuts(app).await?)
        }
        Command::AudioShortcutsSave { settings } => {
            crate::native_shortcuts::audio_shortcuts_save(app, settings).await?;
            Ok(serde_json::Value::Null)
        }
        Command::OverlayEnabled {} => serde_json::to_value(crate::native_overlay::overlay_enabled(
            app.clone(),
            app.state(),
        )?),
        Command::OverlayEnable { enabled } => {
            crate::native_overlay::overlay_enable(app.state(), enabled);
            Ok(serde_json::Value::Null)
        }
        Command::AudioDiagnosticsEnabled {} => {
            serde_json::to_value(audio::audio_diagnostics_enabled().await?)
        }
        Command::AudioDiagnosticsEnable { enabled } => {
            audio::audio_diagnostics_enable(enabled).await?;
            Ok(serde_json::Value::Null)
        }
        Command::AudioDiagnosticsDownload {} => {
            // Export remains useful when device/processing control is wedged.
            app.state::<audio::AudioState>()
                .engine
                .diagnostic_snapshot();
            let settings = app
                .state::<crate::native_voice::VoiceState>()
                .settings
                .try_lock()
                .ok()
                .map(|s| s.clone());
            let (status, status_error) = match audio::audio_status(app.clone(), app.state()).await {
                Ok(mut status) => {
                    status.recording = None;
                    (Some(status), None)
                }
                Err(error) => (None, Some(error)),
            };
            Ok(tauri::async_runtime::spawn_blocking(move || {
                let files = diagnostics::export()?;
                let ui_performance = crate::native_performance::snapshot();
                Ok::<_, String>(serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"settings":settings,"status":status,"status_error":status_error,"ui_performance":ui_performance,"files":files}))
            }).await.map_err(|_| "Diagnostic export failed")??)
        }
        Command::VideoDiagnostics {} => {
            serde_json::to_value(crate::native_video_diagnostics::collect(app).await?)
        }
    };
    value.map_err(|_| "Cannot serialize audio response".into())
}

/// Run serialization off the voice signaling/RTP loop, including large log exports.
pub fn encode(result: Reply) -> std::collections::VecDeque<String> {
    let mut text = serde_json::to_string(&result)
        .unwrap_or_else(|_| r#"{"Err":"Invalid desktop response"}"#.into());
    if text.len() > thiscord_shared::remote_audio::MAX_REPLY_BYTES {
        text = r#"{"Err":"Diagnostic export is too large"}"#.into();
    }
    chunks(&text).into()
}

/// Keep each signaling frame small, including JSON escaping and multibyte text.
pub fn chunks(text: &str) -> Vec<String> {
    let mut remaining = text;
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let mut end = remaining
            .len()
            .min(thiscord_shared::remote_audio::CHUNK_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        result.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn chunks_preserve_utf8_and_stay_bounded() {
        let text = "🎙\"\n".repeat(20000);
        let chunks = super::chunks(&text);
        assert_eq!(chunks.concat(), text);
        assert!(
            chunks
                .iter()
                .all(|c| c.len() <= thiscord_shared::remote_audio::CHUNK_BYTES)
        );
    }
}
