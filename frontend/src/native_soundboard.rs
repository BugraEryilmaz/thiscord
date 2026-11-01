//! Soundboard plays for the connected guild call. The UI only names a sound; PCM
//! is downloaded here with the call's credential and never crosses Tauri IPC.
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{Manager, State};
use thiscord_frontend::audio::{AudioEngine, Command, clips::Clip, connection::Connection};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, SoundId,
    soundboard::{MAX_PCM_BYTES, audio_path, decode_pcm},
};
use tokio::sync::mpsc;

/// Decoded clips kept for this call; about 0.5 MB each at most.
const CACHE: usize = 32;
const FETCHES: usize = 4;
/// Plays of one sound waiting for its download. The mixer overlaps at most four
/// clips (docs/soundboard.md); a larger simultaneous burst is intentionally dropped.
const WAITING: usize = 4;
/// A sound that arrives later than this would be out of context; it is only cached.
const LATE: Duration = Duration::from_secs(3);
/// Local per-person soundboard levels in percent, like voice volume. Only
/// non-default levels are stored, bounded so the file stays small.
const DEFAULT_VOLUME: u16 = 100;
const MAX_VOLUME: u16 = 200;
const VOLUMES: usize = 1000;
const VOLUMES_FILE: &str = "soundboard-volumes.json";

#[derive(Default)]
pub struct SoundboardState {
    requests: Mutex<Option<(GuildId, ChannelId, mpsc::Sender<SoundId>)>>,
    cache: Mutex<(HashMap<SoundId, Clip>, VecDeque<SoundId>)>,
    /// None until loaded from this installation's config directory.
    volumes: Mutex<Option<BTreeMap<AccountId, u16>>>,
    /// Serializes read-modify-write updates of the volumes file.
    file: Mutex<()>,
}

impl SoundboardState {
    fn cached(&self, sound: SoundId) -> Option<Clip> {
        self.cache.lock().ok()?.0.get(&sound).cloned()
    }
    fn insert(&self, sound: SoundId, clip: Clip) {
        if let Ok(mut cache) = self.cache.lock() {
            let (clips, order) = &mut *cache;
            if clips.insert(sound, clip).is_none() {
                order.push_back(sound);
            }
            while order.len() > CACHE {
                if let Some(oldest) = order.pop_front() {
                    clips.remove(&oldest);
                }
            }
        }
    }
    /// This device's level for a person's plays. Zero means their plays are
    /// ignored before any download.
    pub fn gain(&self, account: AccountId) -> f32 {
        let percent = self
            .volumes
            .lock()
            .ok()
            .and_then(|volumes| volumes.as_ref()?.get(&account).copied())
            .unwrap_or(DEFAULT_VOLUME);
        f32::from(percent.min(MAX_VOLUME)) / 100.0
    }
    /// Blocking file read; call from a blocking worker.
    pub fn load_volumes(&self, app: &tauri::AppHandle) -> Result<BTreeMap<AccountId, u16>, String> {
        let mut volumes = self
            .volumes
            .lock()
            .map_err(|_| "Soundboard settings unavailable")?;
        if volumes.is_none() {
            let path = volumes_path(app)?;
            let loaded: BTreeMap<AccountId, u16> = match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice::<BTreeMap<AccountId, u16>>(&bytes)
                    .map(|map| {
                        map.into_iter()
                            .filter(|(_, v)| *v <= MAX_VOLUME && *v != DEFAULT_VOLUME)
                            .take(VOLUMES)
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(_) => return Err("Soundboard settings unavailable".into()),
            };
            *volumes = Some(loaded);
        }
        Ok(volumes.clone().unwrap_or_default())
    }
    pub fn clear(&self) {
        if let Ok(mut requests) = self.requests.lock() {
            *requests = None;
        }
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Default::default();
        }
    }
}

fn volumes_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|_| "Settings directory unavailable")?
        .join(VOLUMES_FILE))
}

/// This device's soundboard level for one person, in percent (0-200).
#[tauri::command]
pub async fn soundboard_volume(
    app: tauri::AppHandle,
    account_id: AccountId,
) -> Result<u16, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(app
            .state::<SoundboardState>()
            .load_volumes(&app)?
            .get(&account_id)
            .copied()
            .unwrap_or(DEFAULT_VOLUME))
    })
    .await
    .map_err(|_| "Soundboard settings unavailable".to_string())?
}

/// Sets one person's soundboard level on this device only; 0% ignores their plays.
#[tauri::command]
pub async fn soundboard_set_volume(
    app: tauri::AppHandle,
    account_id: AccountId,
    volume: u16,
) -> Result<u16, String> {
    if volume > MAX_VOLUME {
        return Err("Volume must be between 0 and 200%".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<SoundboardState>();
        let _file = state
            .file
            .lock()
            .map_err(|_| "Soundboard settings unavailable")?;
        let mut volumes = state.load_volumes(&app)?;
        if volume == DEFAULT_VOLUME {
            volumes.remove(&account_id);
        } else {
            if volumes.len() >= VOLUMES && !volumes.contains_key(&account_id) {
                return Err(
                    "Too many custom soundboard volumes. Reset someone to 100% first.".into(),
                );
            }
            volumes.insert(account_id, volume);
        }
        let path = volumes_path(&app)?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).map_err(|_| "Cannot save soundboard settings")?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(
            &temporary,
            serde_json::to_vec(&volumes).map_err(|_| "Cannot save soundboard settings")?,
        )
        .and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|_| "Cannot save soundboard settings")?;
        *state
            .volumes
            .lock()
            .map_err(|_| "Soundboard settings unavailable")? = Some(volumes);
        Ok(volume)
    })
    .await
    .map_err(|_| "Soundboard settings unavailable".to_string())?
}

/// Plays a sound for everyone in the connected call, if the server allows it.
#[tauri::command]
pub fn voice_soundboard_play(
    state: State<'_, SoundboardState>,
    guild_id: GuildId,
    channel_id: ChannelId,
    sound_id: SoundId,
) -> Result<(), String> {
    let requests = state
        .requests
        .lock()
        .map_err(|_| "Soundboard unavailable")?;
    match requests.as_ref() {
        Some((guild, channel, sender)) if *guild == guild_id && *channel == channel_id => sender
            .try_send(sound_id)
            .map_err(|_| "Wait a moment before playing another sound".into()),
        _ => Err("Join this voice channel to use its soundboard".into()),
    }
}

/// Accepts play requests while this negotiated call is current.
pub(crate) struct Registration<'a> {
    state: &'a SoundboardState,
}
impl<'a> Registration<'a> {
    pub fn new(
        state: &'a SoundboardState,
        guild: GuildId,
        channel: ChannelId,
    ) -> (Self, mpsc::Receiver<SoundId>) {
        let (sender, receiver) = mpsc::channel(2);
        if let Ok(mut requests) = state.requests.lock() {
            *requests = Some((guild, channel, sender));
        }
        (Self { state }, receiver)
    }
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Ok(mut requests) = self.state.requests.lock() {
            *requests = None;
        }
    }
}

type Fetched = (SoundId, Result<Clip, String>);

pub(crate) struct Player {
    client: Option<reqwest::Client>,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    /// Plays (time, player) waiting for each download in flight.
    pending: HashMap<SoundId, Vec<(Instant, AccountId)>>,
    pub tasks: tokio::task::JoinSet<Fetched>,
}

impl Player {
    pub fn new(token: &str, guild: GuildId, channel: ChannelId) -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .build()
                .ok(),
            token: token.into(),
            guild,
            channel,
            pending: HashMap::new(),
            tasks: tokio::task::JoinSet::new(),
        }
    }

    /// A participant played a sound; cached clips start immediately at this
    /// device's level for that person. A 0% level skips the download too.
    pub fn played(
        &mut self,
        state: &SoundboardState,
        engine: &AudioEngine,
        connection: &Connection,
        sound: SoundId,
        account: AccountId,
    ) {
        let gain = state.gain(account);
        if gain <= 0.0 {
            return;
        }
        if let Some(clip) = state.cached(sound) {
            engine.notify(Command::Soundboard {
                connection: connection.clone(),
                clip,
                gain,
            });
            return;
        }
        if let Some(waiting) = self.pending.get_mut(&sound) {
            // Expired plays would never be mixed; never let them crowd out a fresh one.
            waiting.retain(|(at, _)| at.elapsed() < LATE);
            if waiting.len() < WAITING {
                waiting.push((Instant::now(), account));
            }
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.tasks.len() >= FETCHES {
            return;
        }
        self.pending.insert(sound, vec![(Instant::now(), account)]);
        let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
        let url = format!(
            "{}{}",
            base.trim_end_matches('/'),
            audio_path(self.guild, self.channel, sound)
        );
        let token = self.token.clone();
        self.tasks
            .spawn(async move { (sound, fetch(client, url, token).await) });
    }

    pub fn fetched(
        &mut self,
        state: &SoundboardState,
        engine: &AudioEngine,
        connection: &Connection,
        result: Result<Fetched, tokio::task::JoinError>,
        listening: bool,
    ) {
        let Ok((sound, result)) = result else {
            return;
        };
        let waiting = self.pending.remove(&sound).unwrap_or_default();
        match result {
            Ok(clip) => {
                state.insert(sound, clip.clone());
                for (at, account) in waiting {
                    let gain = state.gain(account);
                    if listening && at.elapsed() < LATE && gain > 0.0 {
                        engine.notify(Command::Soundboard {
                            connection: connection.clone(),
                            clip: clip.clone(),
                            gain,
                        });
                    }
                }
            }
            Err(reason) => connection.event(
                "soundboard_fetch_failed",
                serde_json::json!({ "reason": reason }),
            ),
        }
    }
}

async fn fetch(client: reqwest::Client, url: String, token: String) -> Result<Clip, String> {
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "request")?;
    if !response.status().is_success() {
        return Err(format!("status_{}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PCM_BYTES as u64)
    {
        return Err("too_large".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "body")? {
        if body.len() + chunk.len() > MAX_PCM_BYTES {
            return Err("too_large".into());
        }
        body.extend_from_slice(&chunk);
    }
    // Conversion happens on this async task, never in the device callback.
    decode_pcm(&body)
        .map(Clip::from)
        .ok_or_else(|| "invalid_pcm".into())
}
