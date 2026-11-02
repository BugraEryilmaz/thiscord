//! Soundboard in browser calls (docs/soundboard.md). The UI names a sound;
//! `ClientEvent::Soundboard` asks the SFU to play it. Relayed plays download
//! the clip from the same authenticated HTTP API as the desktop (the session
//! cookie authenticates; no token is exposed), validate the 16-bit PCM and
//! play it through the call's WebAudio graph at this browser's per-person
//! level, master volume and output device. Clips are cached for the call.
use super::call::{Call, now};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use thiscord_frontend::browser_audio::{
    self as rules, PendingPlays, PlayRequest, SoundCache, SoundboardVolumes,
};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, SoundId,
    soundboard::{MAX_PCM_BYTES, SAMPLE_RATE, audio_path, decode_pcm},
};
use web_sys::AudioBuffer;

const VOLUMES_KEY: &str = "thiscord.browser-soundboard-volumes.v1";
const FETCH_TIMEOUT_MS: u32 = 10_000;

thread_local! {
    static VOLUMES: RefCell<Option<SoundboardVolumes>> = const { RefCell::new(None) };
}
fn with_volumes<T>(f: impl FnOnce(&mut SoundboardVolumes) -> T) -> T {
    VOLUMES.with(|volumes| {
        let mut volumes = volumes.borrow_mut();
        let volumes = volumes.get_or_insert_with(|| {
            let stored = super::storage().and_then(|s| s.get_item(VOLUMES_KEY).ok().flatten());
            SoundboardVolumes::parse(stored.as_deref())
        });
        f(volumes)
    })
}
/// This browser's level for a person's plays, in percent (0–200).
pub(super) fn volume(account: AccountId) -> u16 {
    with_volumes(|volumes| volumes.get(account))
}
pub(super) fn set_volume(account: AccountId, volume: u16) -> Result<u16, String> {
    let text = with_volumes(|volumes| {
        volumes.set(account, volume)?;
        Ok::<_, &str>(volumes.to_json())
    })?;
    super::storage()
        .and_then(|storage| storage.set_item(VOLUMES_KEY, &text).ok())
        .ok_or("Cannot save soundboard settings")?;
    Ok(volume)
}

/// Per-call clip cache, waiting plays and send spacing.
pub(super) struct Sounds {
    cache: RefCell<SoundCache<AudioBuffer>>,
    pending: RefCell<PendingPlays>,
    last_sent: Cell<Option<f64>>,
}
impl Default for Sounds {
    fn default() -> Self {
        Self {
            cache: RefCell::default(),
            pending: RefCell::default(),
            last_sent: Cell::new(None),
        }
    }
}

/// Sends a play request for the current, negotiated guild call.
pub(super) fn play(
    call: &Call,
    guild_id: GuildId,
    channel_id: ChannelId,
    sound_id: SoundId,
) -> Result<(), String> {
    if call.guild_id != guild_id || call.channel_id.get() != channel_id || !call.soundboard() {
        return Err("Join this voice channel to use its soundboard".into());
    }
    let time = now();
    if call
        .sounds
        .last_sent
        .get()
        .is_some_and(|at| time - at < rules::SOUND_SPACING_MS)
    {
        return Err("Wait a moment before playing another sound".into());
    }
    if !call.signal(thiscord_shared::voice::ClientEvent::Soundboard { sound_id }) {
        return Err("Join this voice channel to use its soundboard".into());
    }
    call.sounds.last_sent.set(Some(time));
    Ok(())
}

/// A participant played a sound. Cached clips start immediately; a 0% level
/// skips the download too.
pub(super) fn played(call: &Rc<Call>, sound: SoundId, account: AccountId) {
    if call.deafened() {
        return;
    }
    let gain = rules::soundboard_gain(volume(account));
    if gain <= 0.0 {
        return;
    }
    let cached = call.sounds.cache.borrow().get(sound).cloned();
    if let Some(buffer) = cached {
        call.output.play_clip(&buffer, gain);
        return;
    }
    if call
        .sounds
        .pending
        .borrow_mut()
        .request(sound, account, now())
        != PlayRequest::Fetch
    {
        return;
    }
    let call = call.clone();
    let (guild, channel) = (call.guild_id, call.channel_id.get());
    wasm_bindgen_futures::spawn_local(async move {
        let token = call.token.clone();
        let samples = super::within(FETCH_TIMEOUT_MS, fetch(&token, guild, channel, sound))
            .await
            .flatten();
        let waiting = call.sounds.pending.borrow_mut().finish(sound, now());
        if call.is_closed() {
            return;
        }
        let Some(buffer) = samples.and_then(|s| call.output.buffer(&s, SAMPLE_RATE as f32)) else {
            return;
        };
        call.sounds.cache.borrow_mut().insert(sound, buffer.clone());
        if call.deafened() || !call.connected() {
            return;
        }
        for account in waiting {
            let gain = rules::soundboard_gain(volume(account));
            if gain > 0.0 {
                call.output.play_clip(&buffer, gain);
            }
        }
    });
}

/// Downloads and validates one clip (bounded like the desktop).
async fn fetch(
    token: &str,
    guild: GuildId,
    channel: ChannelId,
    sound: SoundId,
) -> Option<Vec<f32>> {
    let url = format!(
        "{}{}",
        crate::account_client::api_base(),
        audio_path(guild, channel, sound)
    );
    let mut request = gloo_net::http::Request::get(&url);
    // The web client's empty token means the session cookie authenticates.
    if let Some(token) = crate::account_client::bearer(Some(token)) {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let response = request.send().await.ok()?;
    if !response.ok() {
        return None;
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_PCM_BYTES)
    {
        return None;
    }
    let body = response.binary().await.ok()?;
    if body.len() > MAX_PCM_BYTES {
        return None;
    }
    decode_pcm(&body).map(|pcm| rules::pcm_to_float(&pcm))
}
