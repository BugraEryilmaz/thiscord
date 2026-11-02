//! Pure rules for the web client's WebAudio playback/send graph, soundboard,
//! device tests and connection diagnostics (`browser_voice` owns the browser
//! objects). Kept target-independent so they are unit tested natively.
use std::collections::{BTreeMap, VecDeque};
use thiscord_shared::{
    AccountId, SoundId,
    audio::{AudioQuality, NetworkAdaptationDiagnostics, QualityMetrics},
    voice::ROOM_CAPACITY,
};

/// Per-speaker and per-stream playback gain, like the desktop slider (0–200%).
pub const MAX_STREAM_GAIN: f32 = 2.0;
/// Master output volume (0–200%).
pub const MAX_OUTPUT_VOLUME: f32 = 2.0;
/// Microphone boost range (1×–10×).
pub const MAX_MICROPHONE_BOOST: f32 = 10.0;
/// Stored per-speaker volumes are bounded; the oldest entries are dropped first.
pub const MAX_STORED_VOLUMES: usize = 512;
/// Stored per-person soundboard levels, like `soundboard-volumes.json`.
pub const MAX_SOUNDBOARD_VOLUMES: usize = 1000;
pub const DEFAULT_SOUNDBOARD_VOLUME: u16 = 100;
pub const MAX_SOUNDBOARD_VOLUME: u16 = 200;
/// Decoded clips kept for one call. A 5 s clip is about 1 MB as a float
/// `AudioBuffer`, so the browser keeps fewer than the desktop's 32.
pub const SOUND_CACHE: usize = 16;
/// Concurrent clip downloads.
pub const SOUND_FETCHES: usize = 4;
/// Plays of one sound waiting for its download.
pub const SOUND_WAITING: usize = 4;
/// Clips playing at once; a larger burst is dropped (docs/soundboard.md).
pub const SOUND_OVERLAP: usize = 4;
/// A play that arrives later than this would be out of context; only cached.
pub const SOUND_LATE_MS: f64 = 3_000.0;
/// The SFU drops plays closer than one second apart per connection.
pub const SOUND_SPACING_MS: f64 = 1_000.0;
/// Device test lengths, matching the desktop.
pub const MICROPHONE_TEST_MS: f64 = 60_000.0;
pub const PLAYBACK_TEST_MS: f64 = 5_000.0;

fn bounded(value: f32, low: f32, high: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        fallback
    }
}
/// One speaker's or stream's playback gain node value.
pub fn stream_gain(gain: f32) -> f32 {
    bounded(gain, 0.0, MAX_STREAM_GAIN, 0.0)
}
/// Master gain: output volume, silenced while deafened (self or server).
pub fn master_gain(output_volume: f32, deafened: bool) -> f32 {
    if deafened {
        0.0
    } else {
        bounded(output_volume, 0.0, MAX_OUTPUT_VOLUME, 0.0)
    }
}
/// Microphone boost gain node value; invalid values leave the level unchanged.
pub fn microphone_gain(input_gain: f32) -> f32 {
    bounded(input_gain, 1.0, MAX_MICROPHONE_BOOST, 1.0)
}
/// Soundboard clip gain for one person's level in percent.
pub fn soundboard_gain(percent: u16) -> f32 {
    f32::from(percent.min(MAX_SOUNDBOARD_VOLUME)) / 100.0
}

/// Which remote track a playback slot carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSource {
    Microphone,
    /// Screen-share or application audio.
    SystemAudio,
}
impl RemoteSource {
    /// The mixer slot for a member slot: microphones first, then system audio,
    /// matching `MediaKind::mixer_slot` and the native mixer.
    pub fn slot(self, member_slot: usize) -> Option<usize> {
        (member_slot < ROOM_CAPACITY).then_some(match self {
            Self::Microphone => member_slot,
            Self::SystemAudio => ROOM_CAPACITY + member_slot,
        })
    }
    /// Inverse of [`Self::slot`].
    pub fn from_slot(slot: usize) -> Option<(Self, usize)> {
        match slot {
            s if s < ROOM_CAPACITY => Some((Self::Microphone, s)),
            s if s < ROOM_CAPACITY * 2 => Some((Self::SystemAudio, s - ROOM_CAPACITY)),
            _ => None,
        }
    }
}

/// Per-speaker voice and stream-audio volumes, persisted as JSON in
/// localStorage. Keys come from `browser_call::volume_key`, which keeps voice
/// and stream audio of the same person separate. Most recently set last.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VolumeStore(Vec<(String, f32)>);
impl VolumeStore {
    pub fn parse(text: Option<&str>) -> Self {
        let entries = text
            .and_then(|text| serde_json::from_str::<Vec<(String, f32)>>(text).ok())
            .unwrap_or_default();
        let mut store = Self::default();
        for (key, gain) in entries {
            if key.len() < 128 && gain.is_finite() && (0.0..=MAX_STREAM_GAIN).contains(&gain) {
                store.set(key, gain);
            }
        }
        store
    }
    pub fn get(&self, key: &str) -> Option<f32> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, g)| *g)
    }
    pub fn set(&mut self, key: String, gain: f32) {
        self.0.retain(|(k, _)| *k != key);
        self.0.push((key, gain));
        let excess = self.0.len().saturating_sub(MAX_STORED_VOLUMES);
        self.0.drain(..excess);
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.0).unwrap_or_else(|_| "[]".into())
    }
}

/// This browser's per-person soundboard levels in percent. Only non-default
/// levels are stored; the desktop's limit and messages apply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundboardVolumes(BTreeMap<AccountId, u16>);
impl SoundboardVolumes {
    pub fn parse(text: Option<&str>) -> Self {
        Self(
            text.and_then(|text| serde_json::from_str::<BTreeMap<AccountId, u16>>(text).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, v)| *v <= MAX_SOUNDBOARD_VOLUME && *v != DEFAULT_SOUNDBOARD_VOLUME)
                .take(MAX_SOUNDBOARD_VOLUMES)
                .collect(),
        )
    }
    pub fn get(&self, account: AccountId) -> u16 {
        self.0
            .get(&account)
            .copied()
            .unwrap_or(DEFAULT_SOUNDBOARD_VOLUME)
    }
    pub fn set(&mut self, account: AccountId, volume: u16) -> Result<(), &'static str> {
        if volume > MAX_SOUNDBOARD_VOLUME {
            return Err("Volume must be between 0 and 200%");
        }
        if volume == DEFAULT_SOUNDBOARD_VOLUME {
            self.0.remove(&account);
        } else {
            if self.0.len() >= MAX_SOUNDBOARD_VOLUMES && !self.0.contains_key(&account) {
                return Err("Too many custom soundboard volumes. Reset someone to 100% first.");
            }
            self.0.insert(account, volume);
        }
        Ok(())
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.0).unwrap_or_else(|_| "{}".into())
    }
}

/// Decoded clips for one call, evicting the oldest insertion.
#[derive(Debug)]
pub struct SoundCache<T>(VecDeque<(SoundId, T)>);
impl<T> Default for SoundCache<T> {
    fn default() -> Self {
        Self(VecDeque::new())
    }
}
impl<T> SoundCache<T> {
    pub fn get(&self, sound: SoundId) -> Option<&T> {
        self.0.iter().find(|(id, _)| *id == sound).map(|(_, c)| c)
    }
    pub fn insert(&mut self, sound: SoundId, clip: T) {
        self.0.retain(|(id, _)| *id != sound);
        self.0.push_back((sound, clip));
        while self.0.len() > SOUND_CACHE {
            self.0.pop_front();
        }
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What to do with a relayed play of an uncached sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayRequest {
    /// Start downloading the clip.
    Fetch,
    /// A download is already in flight; play when it finishes.
    Wait,
    /// Over a bound; ignore this play.
    Drop,
}
/// Plays waiting for their clip downloads, with the desktop's bounds.
#[derive(Debug, Default)]
pub struct PendingPlays(Vec<(SoundId, Vec<(f64, AccountId)>)>);
impl PendingPlays {
    pub fn request(&mut self, sound: SoundId, account: AccountId, now_ms: f64) -> PlayRequest {
        if let Some((_, waiting)) = self.0.iter_mut().find(|(id, _)| *id == sound) {
            // Expired plays would never start; never let them crowd out a fresh one.
            waiting.retain(|(at, _)| now_ms - *at < SOUND_LATE_MS);
            if waiting.len() < SOUND_WAITING {
                waiting.push((now_ms, account));
                return PlayRequest::Wait;
            }
            return PlayRequest::Drop;
        }
        if self.0.len() >= SOUND_FETCHES {
            return PlayRequest::Drop;
        }
        self.0.push((sound, vec![(now_ms, account)]));
        PlayRequest::Fetch
    }
    /// Ends a download; returns who played it recently enough to start now.
    pub fn finish(&mut self, sound: SoundId, now_ms: f64) -> Vec<AccountId> {
        let Some(index) = self.0.iter().position(|(id, _)| *id == sound) else {
            return vec![];
        };
        self.0
            .swap_remove(index)
            .1
            .into_iter()
            .filter(|(at, _)| now_ms - *at < SOUND_LATE_MS)
            .map(|(_, account)| account)
            .collect()
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Converts validated 16-bit PCM (`soundboard::decode_pcm`) to WebAudio floats.
pub fn pcm_to_float(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|s| f32::from(*s) / 32_768.0).collect()
}

/// The playback test: alternating 440/660 Hz beeps with soft edges, the same
/// length as the desktop test, at -12 dBFS before output volume.
pub fn test_tone(sample_rate: u32) -> Vec<f32> {
    let rate = sample_rate.clamp(8_000, 192_000) as f32;
    let total = (rate * (PLAYBACK_TEST_MS as f32 / 1000.0)) as usize;
    let beep = (rate * 0.35) as usize;
    let period = (rate * 0.5) as usize;
    let fade = (rate * 0.01) as usize;
    (0..total)
        .map(|i| {
            let (index, offset) = (i / period, i % period);
            if offset >= beep {
                return 0.0;
            }
            let frequency = if index % 2 == 0 { 440.0 } else { 660.0 };
            let edge = offset.min(beep - offset).min(fade) as f32 / fade.max(1) as f32;
            0.25 * edge * (std::f32::consts::TAU * frequency * offset as f32 / rate).sin()
        })
        .collect()
}

/// Connection measurements from one `RTCPeerConnection.getStats()` pass.
/// Missing values stay unavailable; nothing is estimated.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ConnectionStats {
    /// Selected ICE candidate pair round trip to the SFU.
    pub rtt_ms: Option<f64>,
    /// Highest receive jitter among incoming audio streams.
    pub inbound_jitter_ms: Option<f64>,
    pub inbound_received: Option<u64>,
    pub inbound_lost: Option<i64>,
    /// The SFU's receiver report for this browser's microphone.
    pub uplink_jitter_ms: Option<f64>,
    pub uplink_loss_percent: Option<f64>,
    uplink_rtt_ms: Option<f64>,
}
fn seconds_to_ms(value: Option<f64>) -> Option<f64> {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v * 1000.0)
}
fn max(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}
impl ConnectionStats {
    /// A `candidate-pair` report. Only the nominated, succeeded pair counts.
    pub fn candidate_pair(&mut self, succeeded: bool, nominated: bool, rtt_s: Option<f64>) {
        if succeeded
            && nominated
            && let Some(rtt) = seconds_to_ms(rtt_s)
        {
            self.rtt_ms = Some(rtt);
        }
    }
    /// An audio `inbound-rtp` report (one remote speaker or stream).
    pub fn inbound(&mut self, jitter_s: Option<f64>, received: Option<f64>, lost: Option<f64>) {
        self.inbound_jitter_ms = max(self.inbound_jitter_ms, seconds_to_ms(jitter_s));
        if let Some(received) = received.filter(|v| v.is_finite() && *v >= 0.0) {
            self.inbound_received = Some(self.inbound_received.unwrap_or(0) + received as u64);
        }
        if let Some(lost) = lost.filter(|v| v.is_finite()) {
            self.inbound_lost = Some(self.inbound_lost.unwrap_or(0) + lost as i64);
        }
    }
    /// An audio `remote-inbound-rtp` report (the SFU about this microphone).
    pub fn remote_inbound(
        &mut self,
        rtt_s: Option<f64>,
        jitter_s: Option<f64>,
        fraction_lost: Option<f64>,
    ) {
        self.uplink_rtt_ms = seconds_to_ms(rtt_s).or(self.uplink_rtt_ms);
        self.uplink_jitter_ms = max(self.uplink_jitter_ms, seconds_to_ms(jitter_s));
        if let Some(fraction) = fraction_lost.filter(|v| v.is_finite()) {
            self.uplink_loss_percent = Some(fraction.clamp(0.0, 1.0) * 100.0);
        }
    }
    /// Round trip from ICE, else from the SFU's receiver reports.
    pub fn round_trip_ms(&self) -> Option<f64> {
        self.rtt_ms.or(self.uplink_rtt_ms)
    }
    pub fn inbound_loss_percent(&self) -> Option<f64> {
        let received = self.inbound_received?;
        let lost = self.inbound_lost?.max(0) as u64;
        let total = received.saturating_add(lost);
        (total > 0).then(|| lost as f64 * 100.0 / total as f64)
    }
    /// The shared quality report fields these measurements map to.
    pub fn quality(&self) -> AudioQuality {
        AudioQuality {
            sfu_hop_rtt_ms: self.round_trip_ms(),
            sfu_hop_received_packets: self.inbound_received,
            sfu_hop_lost_packets: self.inbound_lost,
            network: (self.uplink_jitter_ms.is_some() || self.uplink_loss_percent.is_some()).then(
                || NetworkAdaptationDiagnostics {
                    uplink_loss_percent: self.uplink_loss_percent,
                    uplink_jitter_ms: self.uplink_jitter_ms,
                    ..Default::default()
                },
            ),
            metrics: QualityMetrics::default(),
            ..Default::default()
        }
    }
}
fn shown(value: Option<f64>, unit: &str) -> String {
    value
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.1}{unit}"))
        .unwrap_or_else(|| "unavailable".into())
}
/// The "Voice connected" tooltip for browser voice.
pub fn connection_brief(stats: Option<&ConnectionStats>) -> String {
    let Some(stats) = stats else {
        return "Measuring · latency unavailable\nJitter unavailable · loss unavailable".into();
    };
    let rtt = stats.round_trip_ms();
    let received_loss = stats.inbound_loss_percent();
    let jitter = max(stats.inbound_jitter_ms, stats.uplink_jitter_ms);
    let health = if rtt.is_none() {
        "Measuring"
    } else if received_loss.is_some_and(|v| v >= 5.0)
        || stats.uplink_loss_percent.is_some_and(|v| v >= 5.0)
    {
        "Packet loss"
    } else if rtt.is_some_and(|v| v > 250.0) {
        "High latency"
    } else if jitter.is_some_and(|v| v > 30.0) {
        "Unstable"
    } else {
        "Healthy"
    };
    let latency = rtt
        .map(|v| format!("{v:.0} ms server RTT"))
        .unwrap_or_else(|| "latency unavailable".into());
    format!(
        "{health} · {latency}\nJitter in {} · out {}\nLoss received {} · sent {}",
        shown(stats.inbound_jitter_ms, " ms"),
        shown(stats.uplink_jitter_ms, " ms"),
        shown(received_loss, "%"),
        shown(stats.uplink_loss_percent, "%"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T: std::str::FromStr>(n: u128) -> T
    where
        T::Err: std::fmt::Debug,
    {
        format!("00000000-0000-0000-0000-{n:012x}").parse().unwrap()
    }
    fn account(n: u128) -> AccountId {
        id(n)
    }
    fn sound(n: u128) -> SoundId {
        id(n)
    }

    #[test]
    fn gains_allow_boost_above_unity_and_clamp() {
        assert_eq!(stream_gain(2.0), 2.0);
        assert_eq!(stream_gain(1.5), 1.5);
        assert_eq!(stream_gain(3.0), 2.0);
        assert_eq!(stream_gain(-1.0), 0.0);
        assert_eq!(stream_gain(f32::NAN), 0.0);
        assert_eq!(master_gain(2.0, false), 2.0);
        assert_eq!(master_gain(2.0, true), 0.0);
        assert_eq!(master_gain(f32::INFINITY, false), 0.0);
        assert_eq!(microphone_gain(4.0), 4.0);
        assert_eq!(microphone_gain(0.5), 1.0);
        assert_eq!(microphone_gain(20.0), 10.0);
        assert_eq!(microphone_gain(f32::NAN), 1.0);
        assert_eq!(soundboard_gain(150), 1.5);
        assert_eq!(soundboard_gain(500), 2.0);
        assert_eq!(soundboard_gain(0), 0.0);
    }

    #[test]
    fn remote_sources_map_to_native_mixer_slots() {
        use thiscord_shared::voice::MediaKind;
        for member in 0..ROOM_CAPACITY {
            assert_eq!(
                RemoteSource::Microphone.slot(member),
                MediaKind::Microphone.mixer_slot(member)
            );
            assert_eq!(
                RemoteSource::SystemAudio.slot(member),
                MediaKind::SystemAudio.mixer_slot(member)
            );
            for source in [RemoteSource::Microphone, RemoteSource::SystemAudio] {
                let slot = source.slot(member).unwrap();
                assert_eq!(RemoteSource::from_slot(slot), Some((source, member)));
            }
        }
        assert_eq!(RemoteSource::Microphone.slot(ROOM_CAPACITY), None);
        assert_eq!(RemoteSource::from_slot(ROOM_CAPACITY * 2), None);
    }

    #[test]
    fn volume_store_is_bounded_recent_and_validated() {
        let mut store = VolumeStore::parse(Some(r#"[["a",1.5],["b",9.0],["c",-1],["d",0.0]]"#));
        assert_eq!(store.get("a"), Some(1.5));
        assert_eq!(store.get("b"), None);
        assert_eq!(store.get("c"), None);
        assert_eq!(store.get("d"), Some(0.0));
        store.set("a".into(), 2.0);
        assert_eq!(VolumeStore::parse(Some(&store.to_json())), store);
        for i in 0..MAX_STORED_VOLUMES + 5 {
            store.set(format!("k{i}"), 1.0);
        }
        assert_eq!(store.0.len(), MAX_STORED_VOLUMES);
        assert_eq!(store.get("a"), None, "oldest entries are dropped first");
        assert_eq!(VolumeStore::parse(Some("garbage")), VolumeStore::default());
    }

    #[test]
    fn soundboard_volumes_store_only_custom_levels_within_bounds() {
        let mut volumes = SoundboardVolumes::default();
        assert_eq!(volumes.get(account(1)), 100);
        volumes.set(account(1), 0).unwrap();
        volumes.set(account(2), 150).unwrap();
        assert!(volumes.set(account(3), 201).is_err());
        let restored = SoundboardVolumes::parse(Some(&volumes.to_json()));
        assert_eq!(restored.get(account(1)), 0);
        assert_eq!(restored.get(account(2)), 150);
        volumes.set(account(2), 100).unwrap();
        assert_eq!(volumes.0.len(), 1, "default levels are removed");
        let mut full = SoundboardVolumes::default();
        for i in 0..MAX_SOUNDBOARD_VOLUMES as u128 {
            full.set(account(i + 10), 50).unwrap();
        }
        assert!(full.set(account(5), 50).is_err());
        assert!(full.set(account(10), 60).is_ok(), "existing entries update");
        assert!(full.set(account(5), 100).is_ok(), "resetting never fails");
    }

    #[test]
    fn sound_cache_evicts_oldest_insertions() {
        let mut cache = SoundCache::default();
        for i in 0..SOUND_CACHE as u128 + 2 {
            cache.insert(sound(i), i);
        }
        assert_eq!(cache.len(), SOUND_CACHE);
        assert!(cache.get(sound(0)).is_none());
        assert_eq!(cache.get(sound(2)), Some(&2));
    }

    #[test]
    fn pending_plays_follow_desktop_bounds_and_lateness() {
        let mut pending = PendingPlays::default();
        assert_eq!(
            pending.request(sound(1), account(1), 0.0),
            PlayRequest::Fetch
        );
        for _ in 1..SOUND_WAITING {
            assert_eq!(
                pending.request(sound(1), account(2), 10.0),
                PlayRequest::Wait
            );
        }
        assert_eq!(
            pending.request(sound(1), account(3), 20.0),
            PlayRequest::Drop
        );
        // Expired plays make room for fresh ones.
        assert_eq!(
            pending.request(sound(1), account(4), 5_000.0),
            PlayRequest::Wait
        );
        assert_eq!(pending.finish(sound(1), 5_100.0), vec![account(4)]);
        assert!(pending.finish(sound(1), 5_100.0).is_empty());
        for i in 0..SOUND_FETCHES as u128 {
            assert_eq!(
                pending.request(sound(10 + i), account(1), 0.0),
                PlayRequest::Fetch
            );
        }
        assert_eq!(
            pending.request(sound(99), account(1), 0.0),
            PlayRequest::Drop
        );
        assert!(
            pending.finish(sound(10), 4_000.0).is_empty(),
            "late plays only cache"
        );
    }

    #[test]
    fn pcm_and_test_tone_are_bounded() {
        assert_eq!(pcm_to_float(&[0, i16::MIN, 16_384]), vec![0.0, -1.0, 0.5]);
        let tone = test_tone(48_000);
        assert_eq!(tone.len(), 240_000);
        assert!(tone.iter().all(|s| s.abs() <= 0.25));
        assert!(tone.iter().any(|s| s.abs() > 0.2));
        assert_eq!(tone[0], 0.0, "soft start");
    }

    #[test]
    fn connection_stats_map_reports_and_mark_missing_values() {
        let mut stats = ConnectionStats::default();
        stats.candidate_pair(false, true, Some(0.5));
        stats.candidate_pair(true, false, Some(0.5));
        assert_eq!(stats.round_trip_ms(), None);
        stats.remote_inbound(Some(0.05), Some(0.004), Some(0.02));
        assert_eq!(
            stats.round_trip_ms(),
            Some(50.0),
            "receiver report fallback"
        );
        stats.candidate_pair(true, true, Some(0.042));
        assert_eq!(stats.round_trip_ms(), Some(42.0));
        stats.inbound(Some(0.003), Some(90.0), Some(10.0));
        stats.inbound(Some(0.008), Some(100.0), Some(0.0));
        assert_eq!(stats.inbound_jitter_ms, Some(8.0));
        assert_eq!(stats.inbound_loss_percent(), Some(5.0));
        let quality = stats.quality();
        assert_eq!(quality.sfu_hop_rtt_ms, Some(42.0));
        assert_eq!(quality.sfu_hop_received_packets, Some(190));
        let network = quality.network.unwrap();
        assert_eq!(network.uplink_loss_percent, Some(2.0));
        assert_eq!(network.uplink_jitter_ms, Some(4.0));
        let brief = connection_brief(Some(&stats));
        assert!(
            brief.starts_with("Packet loss · 42 ms server RTT"),
            "{brief}"
        );
        assert!(brief.contains("Jitter in 8.0 ms · out 4.0 ms"), "{brief}");
        let empty = connection_brief(Some(&ConnectionStats::default()));
        assert!(empty.starts_with("Measuring"));
        assert!(empty.contains("Loss received unavailable · sent unavailable"));
        assert!(connection_brief(None).contains("unavailable"));
        let mut healthy = ConnectionStats::default();
        healthy.candidate_pair(true, true, Some(0.02));
        assert!(connection_brief(Some(&healthy)).starts_with("Healthy"));
    }
}
