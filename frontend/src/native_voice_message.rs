//! Desktop voice messages: recording, preview, authenticated upload and
//! playback on the selected devices. Only levels, positions, durations and
//! attachment metadata cross IPC; recordings are uploaded and attachments
//! downloaded here with the session token, never through the WebView.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tauri::{Emitter, Manager, State};
use thiscord_frontend::audio::voice_message::{
    Clip, Command, Level, Outcome, Player, Recorder, Recording, Status,
};
use thiscord_shared::{
    AttachmentId, ChannelId, GuildId,
    chat::{ATTACHMENTS_PATH, ChatAttachment},
    friends::DIRECT_ATTACHMENTS_PATH,
    ids::ConversationId,
    voice_message::{MAX_VOICE_MESSAGE_BYTES, VOICE_MESSAGE_FILENAME, encode_waveform},
};

const RECORDER_EVENT: &str = "voice-message-recorder";
const PLAYER_EVENT: &str = "voice-message-player";
/// Parsed clips kept for replay and seeking; at most 6 × 5 MiB.
const CACHED_CLIPS: usize = 6;
const RECORDING_KEY: &str = "recording";

#[derive(Default)]
pub struct VoiceMessageState {
    recorder: Mutex<Option<Recorder>>,
    recording: Arc<Mutex<Option<Arc<Recording>>>>,
    player: Mutex<Option<Player>>,
    clips: Mutex<VecDeque<(AttachmentId, Arc<Clip>)>>,
    /// Current recording session; late events of older sessions are ignored.
    session: Arc<AtomicU64>,
    /// Bumped by every play request and stop: a play whose download finishes
    /// after a newer play or a stop never starts.
    play: AtomicU64,
}

#[derive(Clone, Serialize)]
struct RecorderEvent {
    /// Returned by `voice_message_record_start`; identifies this recording.
    session: u64,
    /// "recording", "ready", "limit", "too_short", "cancelled" or "error".
    state: &'static str,
    elapsed_ms: u32,
    level: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    waveform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn base() -> &'static str {
    option_env!("THISCORD_API_URL")
        .unwrap_or("http://localhost:3000")
        .trim_end_matches('/')
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| "Cannot prepare the network client".into())
}

fn session(token: &str) -> Result<&str, String> {
    if token.len() == 43 {
        Ok(token)
    } else {
        Err("Sign in again to use voice messages".into())
    }
}

/// Server errors carry a safe, translatable message; never echo bodies otherwise.
async fn failure(response: reqwest::Response) -> String {
    let status = response.status().as_u16();
    match response.bytes().await {
        Ok(body) if body.len() <= 32 * 1024 => {
            serde_json::from_slice::<thiscord_shared::ApiError>(&body)
                .map(|e| e.message)
                .unwrap_or_else(|_| format!("Voice message request failed ({status})"))
        }
        _ => format!("Voice message request failed ({status})"),
    }
}

#[tauri::command]
pub async fn voice_message_record_start(
    app: tauri::AppHandle,
    state: State<'_, VoiceMessageState>,
) -> Result<u64, String> {
    let settings = crate::native_audio::audio_load(app.clone()).await?;
    let session = state.session.fetch_add(1, Ordering::AcqRel) + 1;
    let current = state.session.clone();
    if let Some(previous) = state
        .recorder
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .take()
    {
        previous.cancel();
    }
    stop_preview(&state);
    state
        .recording
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .take();
    let levels_app = app.clone();
    let done_app = app.clone();
    let stored = state.recording.clone();
    let input = settings.input.clone();
    let gain = settings.input_gain;
    let recorder = tauri::async_runtime::spawn_blocking(move || {
        Recorder::start(
            input,
            gain,
            move |level: Level| {
                let _ = levels_app.emit_to(
                    "main",
                    RECORDER_EVENT,
                    RecorderEvent {
                        session,
                        state: "recording",
                        elapsed_ms: level.elapsed_ms,
                        level: level.level,
                        waveform: None,
                        error: None,
                    },
                );
            },
            move |outcome| {
                let (state, recording, error) = match outcome {
                    Outcome::Ready(recording) => ("ready", Some(recording), None),
                    Outcome::Limit(recording) => ("limit", Some(recording), None),
                    Outcome::TooShort => ("too_short", None, None),
                    Outcome::Cancelled => ("cancelled", None, None),
                    Outcome::Failed(error) => ("error", None, Some(error)),
                };
                let (elapsed_ms, waveform) = recording.as_ref().map_or((0, None), |r| {
                    (r.duration_ms, Some(encode_waveform(&r.waveform)))
                });
                // A newer recording owns the slot; never overwrite it.
                if current.load(Ordering::Acquire) == session
                    && let Ok(mut slot) = stored.lock()
                {
                    *slot = recording.map(Arc::new);
                }
                let _ = done_app.emit_to(
                    "main",
                    RECORDER_EVENT,
                    RecorderEvent {
                        session,
                        state,
                        elapsed_ms,
                        level: 0.0,
                        waveform,
                        error,
                    },
                );
            },
        )
    })
    .await
    .map_err(|_| "Recorder task failed")??;
    *state.recorder.lock().map_err(|_| "Recorder unavailable")? = Some(recorder);
    Ok(session)
}

/// Stops recording; the outcome arrives as a recorder event.
#[tauri::command]
pub fn voice_message_record_stop(state: State<'_, VoiceMessageState>) -> Result<(), String> {
    if let Some(recorder) = state
        .recorder
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .take()
    {
        recorder.finish();
    }
    Ok(())
}

#[tauri::command]
pub fn voice_message_record_cancel(state: State<'_, VoiceMessageState>) -> Result<(), String> {
    if let Some(recorder) = state
        .recorder
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .take()
    {
        recorder.cancel();
    }
    stop_preview(&state);
    state
        .recording
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .take();
    Ok(())
}

fn stop_preview(state: &VoiceMessageState) {
    state.play.fetch_add(1, Ordering::AcqRel);
    if let Ok(player) = state.player.lock()
        && let Some(player) = player.as_ref()
    {
        let _ = player.send(Command::Stop);
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UploadTarget {
    Guild {
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Direct {
        conversation_id: ConversationId,
    },
}

/// Uploads the finished recording as a staged voice-message attachment.
/// The recording is kept until a later recording or discard, so a failed
/// upload can be retried.
#[tauri::command]
pub async fn voice_message_upload(
    state: State<'_, VoiceMessageState>,
    token: String,
    target: UploadTarget,
) -> Result<ChatAttachment, String> {
    let token = session(&token)?.to_owned();
    let recording = state
        .recording
        .lock()
        .map_err(|_| "Recorder unavailable")?
        .clone()
        .ok_or("Record a voice message first")?;
    let waveform = encode_waveform(&recording.waveform);
    let common = format!(
        "filename={VOICE_MESSAGE_FILENAME}&content_type=audio%2Fogg&voice=true&waveform={waveform}"
    );
    let url = match target {
        UploadTarget::Guild {
            guild_id,
            channel_id,
        } => format!(
            "{}{ATTACHMENTS_PATH}?guild_id={guild_id}&channel_id={channel_id}&{common}",
            base()
        ),
        UploadTarget::Direct { conversation_id } => format!(
            "{}{DIRECT_ATTACHMENTS_PATH}?conversation_id={conversation_id}&{common}",
            base()
        ),
    };
    let response = client()?
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/octet-stream")
        .body(recording.ogg.clone())
        .send()
        .await
        .map_err(|_| "Upload interrupted. Retry when connected.")?;
    if !response.status().is_success() {
        return Err(failure(response).await);
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| "Invalid upload response")?;
    if body.len() > 32 * 1024 {
        return Err("Invalid upload response".into());
    }
    serde_json::from_slice(&body).map_err(|_| "Invalid upload response".into())
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A published attachment, downloaded with the caller's session.
    Attachment {
        id: AttachmentId,
        #[serde(default)]
        private: bool,
    },
    /// The finished, unsent recording.
    Recording,
}

async fn download(token: &str, id: AttachmentId, private: bool) -> Result<Clip, String> {
    let path = if private {
        DIRECT_ATTACHMENTS_PATH
    } else {
        ATTACHMENTS_PATH
    };
    let mut response = client()?
        .get(format!("{}{path}/{id}", base()))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Download interrupted. Retry when connected.")?;
    if !response.status().is_success() {
        return Err(failure(response).await);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_VOICE_MESSAGE_BYTES as u64)
    {
        return Err("Voice messages must be 5 MiB or smaller".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Download interrupted. Retry when connected.")?
    {
        if body.len() + chunk.len() > MAX_VOICE_MESSAGE_BYTES {
            return Err("Voice messages must be 5 MiB or smaller".into());
        }
        body.extend_from_slice(&chunk);
    }
    tauri::async_runtime::spawn_blocking(move || Clip::parse(&body))
        .await
        .map_err(|_| "Cannot read the voice message")?
}

fn player<'a>(app: &tauri::AppHandle, slot: &'a mut Option<Player>) -> Result<&'a Player, String> {
    if slot.is_none() {
        let events = app.clone();
        *slot = Some(Player::new(move |status: Status| {
            let _ = events.emit_to("main", PLAYER_EVENT, status);
        })?);
    }
    Ok(slot.as_ref().unwrap())
}

#[tauri::command]
pub async fn voice_message_play(
    app: tauri::AppHandle,
    state: State<'_, VoiceMessageState>,
    token: String,
    source: Source,
    position_ms: u32,
    speed: f32,
) -> Result<(), String> {
    let ticket = state.play.fetch_add(1, Ordering::AcqRel) + 1;
    let settings = crate::native_audio::audio_load(app.clone()).await?;
    let speed = if speed.is_finite() {
        speed.clamp(1.0, 2.0)
    } else {
        1.0
    };
    let (key, clip) = match source {
        Source::Recording => {
            let recording = state
                .recording
                .lock()
                .map_err(|_| "Recorder unavailable")?
                .clone()
                .ok_or("Record a voice message first")?;
            let clip = tauri::async_runtime::spawn_blocking(move || Clip::parse(&recording.ogg))
                .await
                .map_err(|_| "Cannot read the voice message")??;
            (RECORDING_KEY.to_owned(), Arc::new(clip))
        }
        Source::Attachment { id, private } => {
            let cached = state
                .clips
                .lock()
                .map_err(|_| "Player unavailable")?
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, clip)| clip.clone());
            let clip = match cached {
                Some(clip) => clip,
                None => {
                    let token = session(&token)?.to_owned();
                    let clip = Arc::new(download(&token, id, private).await?);
                    let mut clips = state.clips.lock().map_err(|_| "Player unavailable")?;
                    clips.retain(|(key, _)| *key != id);
                    if clips.len() >= CACHED_CLIPS {
                        clips.pop_front();
                    }
                    clips.push_back((id, clip.clone()));
                    clip
                }
            };
            (id.to_string(), clip)
        }
    };
    let mut slot = state.player.lock().map_err(|_| "Player unavailable")?;
    if state.play.load(Ordering::Acquire) != ticket {
        return Ok(());
    }
    player(&app, &mut slot)?.send(Command::Play {
        key,
        clip,
        position_ms,
        speed,
        output: settings.output,
        volume: settings.output_volume,
    })
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Pause,
    Resume,
    Stop,
    Seek { position_ms: u32 },
    Speed { speed: f32 },
}

#[tauri::command]
pub fn voice_message_control(
    app: tauri::AppHandle,
    state: State<'_, VoiceMessageState>,
    control: Control,
) -> Result<(), String> {
    let mut slot = state.player.lock().map_err(|_| "Player unavailable")?;
    if matches!(control, Control::Stop) {
        state.play.fetch_add(1, Ordering::AcqRel);
    }
    let player = player(&app, &mut slot)?;
    player.send(match control {
        Control::Pause => Command::Pause,
        Control::Resume => Command::Resume,
        Control::Stop => Command::Stop,
        Control::Seek { position_ms } => Command::Seek(position_ms),
        Control::Speed { speed } if speed.is_finite() => Command::Speed(speed.clamp(1.0, 2.0)),
        Control::Speed { .. } => return Err("Invalid playback speed".into()),
    })
}

/// Clears cached clips and any unsent recording (sign-out).
#[tauri::command]
pub fn voice_message_reset(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<VoiceMessageState>();
    if let Ok(mut recorder) = state.recorder.lock()
        && let Some(recorder) = recorder.take()
    {
        recorder.cancel();
    }
    stop_preview(&state);
    if let Ok(mut recording) = state.recording.lock() {
        recording.take();
    }
    if let Ok(mut clips) = state.clips.lock() {
        clips.clear();
    }
    Ok(())
}
