use super::*;
use audio::{AudioEngine, Command as EngineCommand, connection::Connection, diagnostics};
use thiscord_shared::remote_audio::{CHUNK_BYTES, Command, MAX_REPLY_BYTES, Reply};

pub(super) fn execute(
    engine: AudioEngine,
    connection: Connection,
    bot: Arc<StdMutex<Bot>>,
    command: Command,
) -> Reply {
    command.validate()?;
    if !connection.active() {
        return Err("Bot disconnected".into());
    }
    let value = match command {
        Command::AudioCurrent {} => serde_json::to_value(bot.lock().unwrap().settings.audio.clone()),
        Command::AudioDevices {} => serde_json::to_value(audio::devices()?),
        Command::AudioStatus {} => serde_json::to_value(engine.command(EngineCommand::Status)?),
        Command::AudioSave { settings, expected } => {
            validate_audio(&settings)?;
            if bot.lock().unwrap().settings.audio != expected { return Err("Audio settings changed. Refresh before retrying.".into()); }
            engine.restrict(&settings);
            engine.command(EngineCommand::Settings(settings.clone()))?;
            if !connection.active() { return Err("Bot disconnected".into()); }
            bot.lock().unwrap().settings.audio = settings;
            Ok(serde_json::Value::Null)
        },
        Command::AudioToggle { action } => {
            let mut settings = bot.lock().unwrap().settings.audio.clone();
            action.apply(&mut settings);
            engine.restrict(&settings);
            engine.command(EngineCommand::Settings(settings.clone()))?;
            bot.lock().unwrap().settings.audio = settings;
            Ok(serde_json::Value::Null)
        },
        Command::AudioVolume { stream, gain, target } => serde_json::to_value(engine.command(EngineCommand::Volume { stream, gain, target })?),
        Command::AudioDiagnosticsEnabled {} => serde_json::to_value(diagnostics::enabled()?),
        Command::AudioDiagnosticsEnable { enabled } => { diagnostics::set_enabled(enabled)?; Ok(serde_json::Value::Null) },
        Command::AudioDiagnosticsDownload {} => {
            engine.diagnostic_snapshot();
            let status = engine.command(EngineCommand::Status);
            Ok(serde_json::json!({"version":env!("CARGO_PKG_VERSION"), "settings":bot.lock().unwrap().settings,
                "status":status, "files":diagnostics::export()?}))
        },
        _ => return Err("Keyboard shortcuts and overlays require a physical desktop. Use the bot's virtual PTT control.".into()),
    };
    value.map_err(|_| "Invalid bot audio response".into())
}
pub(super) fn encode(result: Reply) -> std::collections::VecDeque<String> {
    let mut text = serde_json::to_string(&result)
        .unwrap_or_else(|_| r#"{"Err":"Invalid audio response"}"#.into());
    if text.len() > MAX_REPLY_BYTES {
        text = r#"{"Err":"Diagnostic export is too large"}"#.into();
    }
    let mut chunks = std::collections::VecDeque::new();
    let mut remaining = text.as_str();
    while !remaining.is_empty() {
        let mut end = remaining.len().min(CHUNK_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push_back(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    chunks
}

pub(super) struct Logs(std::path::PathBuf);
impl Logs {
    pub fn new(id: AccountId) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("thiscord-bot-{id}"));
        std::fs::create_dir(&path).map_err(|_| "Cannot create bot diagnostic directory")?;
        let logs = Self(path.clone());
        diagnostics::initialize(path.clone(), path.join("preference.json"))?;
        Ok(logs)
    }
}
impl Drop for Logs {
    fn drop(&mut self) {
        let _ = diagnostics::set_enabled(false);
        for name in [
            "preference.json",
            "audio-diagnostics.jsonl",
            "audio-diagnostics.1.jsonl",
            "audio-diagnostics.2.jsonl",
        ] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}
