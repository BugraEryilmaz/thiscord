//! Opt-in, bounded diagnostic capture. Called only on the audio worker, never
//! on CPAL callbacks. A separate thread owns all file I/O and WAV finalization.
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiscord_shared::audio::{AudioRecordingStatus, AudioSettings};

pub const RATE: usize = 48_000;
pub const MAX_SECONDS: usize = 60;
const MAX_SAMPLES: usize = RATE * MAX_SECONDS;
const QUEUE_BLOCKS: usize = 256;
const FILES: [&str; 3] = [
    "speaker-output.wav",
    "microphone-input.wav",
    "transmit-input.wav",
];

#[derive(Clone, Copy)]
pub(super) enum Track {
    Speaker,
    Microphone,
    Transmit,
}
impl Track {
    fn name(self) -> &'static str {
        ["speaker", "microphone", "transmit"][self as usize]
    }
}
struct Block {
    track: Track,
    at: Instant,
    pcm: [f32; 480],
    gap: bool,
    reset: bool,
    delay_ms: i32,
    delivery: Option<(bool, bool)>,
}
#[derive(Clone, Copy)]
pub(super) struct Continuity {
    pub gap: bool,
    pub reset: bool,
}
impl From<bool> for Continuity {
    fn from(gap: bool) -> Self {
        Self { gap, reset: gap }
    }
}
#[allow(
    clippy::large_enum_variant,
    reason = "Keep PCM inline in the bounded queue; avoid a heap allocation for each block"
)]
enum Event {
    Block(Block),
    Settings(AudioSettings),
}
struct State {
    // 0 = recording, 1 = draining/finalizing, 2 = finished (possibly failed).
    phase: AtomicU8,
    end_samples: AtomicU64,
    dropped: AtomicU64,
    error: Mutex<Option<String>>,
    directory: PathBuf,
    origin: Instant,
}
impl State {
    fn error(&self, message: &str) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.into());
    }
}
struct Active {
    tx: mpsc::SyncSender<Event>,
    state: Arc<State>,
}
#[derive(Default)]
pub(super) struct Recorder {
    active: Option<Active>,
    last: Option<Arc<State>>,
}
fn sanitized(mut settings: AudioSettings) -> AudioSettings {
    // Device labels/formats are recorded separately, without persistent OS IDs
    // or a model override path that can expose a local username/home directory.
    settings.input = None;
    settings.output = None;
    settings.neural_echo_model = None;
    settings
}
impl Recorder {
    pub fn start(
        &mut self,
        parent: PathBuf,
        settings: AudioSettings,
        devices: Value,
    ) -> Result<(), String> {
        if self
            .last
            .as_ref()
            .is_some_and(|s| s.phase.load(Ordering::Acquire) != 2)
        {
            return Err("A debug recording is already recording or saving".into());
        }
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock unavailable")?
            .as_nanos();
        let state = Arc::new(State {
            phase: AtomicU8::new(0),
            end_samples: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            error: Mutex::new(None),
            directory: parent.join(format!("echo-{suffix}-{}", std::process::id())),
            origin: Instant::now(),
        });
        let (tx, rx) = mpsc::sync_channel(QUEUE_BLOCKS);
        let writer_state = state.clone();
        let header = json!({"type":"header", "schema":1, "version":env!("CARGO_PKG_VERSION"),
            "os":std::env::consts::OS, "arch":std::env::consts::ARCH,
            "rate":RATE,"channels":1,"max_seconds":MAX_SECONDS,
            "settings":sanitized(settings),"devices":devices});
        std::thread::Builder::new().name("thiscord-audio-recording".into()).spawn(move || {
            if write_session(&writer_state, rx, header).is_err() {
                writer_state.error("Could not save all diagnostic files. Check free disk space and folder permissions; this recording is incomplete.");
            }
            writer_state.phase.store(2, Ordering::Release);
        }).map_err(|_| "Cannot start recording writer")?;
        self.last = Some(state.clone());
        self.active = Some(Active { tx, state });
        Ok(())
    }
    pub fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            active.state.end_samples.store(
                (active.state.origin.elapsed().as_secs_f64() * RATE as f64)
                    .round()
                    .min(MAX_SAMPLES as f64) as u64,
                Ordering::Release,
            );
            let _ = active
                .state
                .phase
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
            // Dropping the sender is a reliable stop, even when the queue is full.
        }
    }
    pub fn poll(&mut self) {
        if self.active.as_ref().is_some_and(|a| {
            a.state.origin.elapsed() >= Duration::from_secs(MAX_SECONDS as u64)
                || a.state.phase.load(Ordering::Acquire) != 0
        }) {
            self.stop();
        }
    }
    fn send(&mut self, event: Event) {
        self.poll();
        if let Some(a) = &self.active {
            match a.tx.try_send(event) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    a.state.dropped.fetch_add(1, Ordering::Relaxed);
                    a.state.error("Recording stopped because the disk writer could not keep up. Files are incomplete.");
                    self.stop();
                }
                Err(mpsc::TrySendError::Disconnected(_)) => self.stop(),
            }
        }
    }
    pub fn block(
        &mut self,
        track: Track,
        at: Instant,
        pcm: &[f32; 480],
        continuity: Continuity,
        delay_ms: i32,
        delivery: Option<(bool, bool)>,
    ) {
        if self.active.is_some() {
            self.send(Event::Block(Block {
                track,
                at,
                pcm: *pcm,
                gap: continuity.gap,
                reset: continuity.reset,
                delay_ms,
                delivery,
            }));
        }
    }
    pub fn settings(&mut self, settings: &AudioSettings) {
        if self.active.is_some() {
            self.send(Event::Settings(sanitized(settings.clone())));
        }
    }
    pub fn status(&self) -> Option<AudioRecordingStatus> {
        self.last.as_ref().map(|s| {
            let phase = s.phase.load(Ordering::Acquire);
            AudioRecordingStatus {
                active: phase == 0,
                saving: phase == 1,
                elapsed_ms: if phase == 0 {
                    s.origin.elapsed().as_millis().min(60_000) as u64
                } else {
                    s.end_samples.load(Ordering::Acquire) * 1000 / RATE as u64
                },
                directory: s.directory.to_string_lossy().into_owned(),
                dropped_blocks: s.dropped.load(Ordering::Relaxed),
                error: s.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            }
        })
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
struct Wave {
    writer: hound::WavWriter<BufWriter<File>>,
    samples: usize,
    blocks: u64,
}
impl Wave {
    fn new(path: &Path) -> Result<Self, hound::Error> {
        Ok(Self {
            writer: hound::WavWriter::new(
                BufWriter::with_capacity(65536, private_file(path)?),
                hound::WavSpec {
                    channels: 1,
                    sample_rate: RATE as u32,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )?,
            samples: 0,
            blocks: 0,
        })
    }
    fn pad(&mut self, until: usize) -> Result<(), hound::Error> {
        while self.samples < until.min(MAX_SAMPLES) {
            self.writer.write_sample(0.0_f32)?;
            self.samples += 1;
        }
        Ok(())
    }
    fn block(&mut self, block: &Block, origin: Instant) -> Result<Value, hound::Error> {
        let relative = if block.at >= origin {
            block.at.duration_since(origin).as_secs_f64()
        } else {
            -origin.duration_since(block.at).as_secs_f64()
        };
        let observed_sample = (relative * RATE as f64).round() as i64;
        let prefix = observed_sample.saturating_neg().clamp(0, 480) as usize;
        // Preserve every sample in a continuous device stream instead of
        // trimming/duplicating samples due to callback timestamp jitter. Each
        // block also retains its original timestamp for clock-drift analysis.
        if self.blocks == 0 || block.gap {
            self.pad(observed_sample.max(0) as usize)?;
        }
        let offset = self.samples;
        let count = (480 - prefix).min(MAX_SAMPLES.saturating_sub(offset));
        for &sample in &block.pcm[prefix..prefix + count] {
            self.writer.write_sample(sample)?;
        }
        self.samples += count;
        if count > 0 {
            self.blocks += 1;
        }
        Ok(
            json!({"type":"block","track":block.track.name(),"offset":offset,"count":count,
            "prefix":prefix,"observed_sample":observed_sample,"gap":block.gap,"reset":block.reset,"delay_ms":block.delay_ms,
            "accepted":block.delivery.map(|d|d.0),"gate_open":block.delivery.map(|d|d.1)}),
        )
    }
}
fn write_session(
    state: &State,
    rx: mpsc::Receiver<Event>,
    header: Value,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(state.directory.parent().ok_or("Missing recording parent")?)?;
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(&state.directory)?;
    let mut metadata = BufWriter::new(private_file(&state.directory.join("timeline.jsonl"))?);
    writeln!(metadata, "{header}")?;
    std::fs::write(state.directory.join("README.txt"), README)?;
    let mut waves = FILES
        .map(|name| Wave::new(&state.directory.join(name)))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let mut last_flush = Instant::now();
    for event in rx {
        match event {
            Event::Block(block) => {
                let event = waves[block.track as usize].block(&block, state.origin)?;
                writeln!(metadata, "{event}")?;
            }
            Event::Settings(settings) => writeln!(
                metadata,
                "{}",
                json!({"type":"settings","settings":settings})
            )?,
        }
        if last_flush.elapsed() >= Duration::from_secs(1) {
            for wave in &mut waves {
                wave.writer.flush()?;
            }
            metadata.flush()?;
            last_flush = Instant::now();
        }
    }
    let length = waves
        .iter()
        .map(|w| w.samples)
        .max()
        .unwrap_or(0)
        .max(state.end_samples.load(Ordering::Acquire) as usize)
        .min(MAX_SAMPLES);
    for mut wave in waves {
        wave.pad(length)?;
        wave.writer.finalize()?;
    }
    writeln!(
        metadata,
        "{}",
        json!({"type":"end","samples":length,
        "dropped_blocks":state.dropped.load(Ordering::Relaxed),
        "complete":state.dropped.load(Ordering::Relaxed)==0})
    )?;
    metadata.flush()?;
    Ok(())
}
const README: &str = "Thiscord echo diagnostic recording\n\n\
Share the entire folder, including timeline.jsonl, only with people you trust.\n\
Recordings include raw microphone audio even while muted and other participants.\n\
No audio is automatically uploaded. Delete this folder when no longer needed.\n\n\
All WAVs: 48 kHz, mono, 32-bit float, common start and padded end.\n\
speaker-output.wav: Thiscord post-volume/clipping/deafen playback reference,\n\
before device conversion/OS volume. Not system-wide audio or measured room sound.\n\
microphone-input.wav: mono device input before Thiscord DSP and mute/PTT.\n\
transmit-input.wav: after DSP and mute/PTT/VAD, immediately before Opus encoding.\n\
It does not include codec/network loss. accepted in timeline records admission\n\
to the voice transport queue, not confirmation of server or listener delivery.\n\n\
Timeline preserves worker processing order, capture/playback timestamps, queue\n\
gaps and settings. Continuous samples are not trimmed for timestamp jitter.\n\
Input and output clocks may drift; inspect observed_sample for precise analysis.\n\
A missing end record or complete=false means the recording is incomplete.\n\
DSP was already adapting when capture began; replay starts fresh. Use the first\n\
5-10 seconds to warm up before comparing echo/double-talk. Room/device behavior\n\
is captured in the signals, but cannot be recreated by an offline replay.\n";

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "thiscord-record-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn done(state: &State) {
        let start = Instant::now();
        while state.phase.load(Ordering::Acquire) != 2 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "writer did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn samples(path: &Path) -> Vec<f32> {
        let reader = hound::WavReader::open(path).unwrap();
        assert_eq!(reader.spec().sample_rate, 48000);
        assert_eq!(reader.spec().channels, 1);
        reader
            .into_samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
    #[test]
    fn synchronized_wavs_preserve_raw_and_gated_audio_and_sanitized_metadata() {
        let parent = directory();
        let mut recorder = Recorder::default();
        let settings = AudioSettings {
            input: Some("private-device-id".into()),
            neural_echo_model: Some("/private/home/model.tflite".into()),
            ..Default::default()
        };
        recorder.start(parent.clone(), settings, json!({})).unwrap();
        let state = recorder.last.as_ref().unwrap().clone();
        let at = state.origin;
        recorder.block(Track::Speaker, at, &[0.25; 480], false.into(), 0, None);
        recorder.block(Track::Microphone, at, &[0.5; 480], false.into(), 40, None);
        recorder.block(
            Track::Transmit,
            at,
            &[0.0; 480],
            false.into(),
            0,
            Some((true, false)),
        );
        let second = at + Duration::from_millis(10);
        recorder.block(
            Track::Microphone,
            second,
            &[0.7; 480],
            false.into(),
            50,
            None,
        );
        recorder.block(
            Track::Transmit,
            second,
            &[0.1; 480],
            false.into(),
            0,
            Some((false, true)),
        );
        recorder.settings(&AudioSettings {
            muted: true,
            ..Default::default()
        });
        recorder.stop();
        done(&state);
        let status = recorder.status().unwrap();
        assert!(!status.active && !status.saving && status.error.is_none());
        let tracks = FILES.map(|f| samples(&state.directory.join(f)));
        assert!(
            tracks
                .iter()
                .all(|s| s.len() == tracks[0].len() && s.len() >= 960)
        );
        assert_eq!(&tracks[0][..480], &[0.25; 480]);
        assert_eq!(&tracks[1][..480], &[0.5; 480]);
        assert_eq!(&tracks[2][..480], &[0.0; 480]);
        assert_eq!(&tracks[1][480..960], &[0.7; 480]);
        assert_eq!(&tracks[2][480..960], &[0.1; 480]);
        let timeline = std::fs::read_to_string(state.directory.join("timeline.jsonl")).unwrap();
        assert!(!timeline.contains("private-device-id") && !timeline.contains("/private/home"));
        let events: Vec<Value> = timeline
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(events[3]["gate_open"], false);
        assert_eq!(events[5]["accepted"], false);
        assert_eq!(events.last().unwrap()["complete"], true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(state.directory.join(FILES[0]))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(parent).unwrap();
    }
    #[test]
    fn timestamp_prefix_gap_and_jitter_keep_samples_and_enforce_duration_bound() {
        let parent = directory();
        std::fs::create_dir(&parent).unwrap();
        let mut wave = Wave::new(&parent.join("test.wav")).unwrap();
        let origin = Instant::now();
        let mut block = Block {
            track: Track::Microphone,
            at: origin - Duration::from_millis(5),
            pcm: [0.1; 480],
            gap: false,
            reset: false,
            delay_ms: 0,
            delivery: None,
        };
        let first = wave.block(&block, origin).unwrap();
        assert_eq!(first["prefix"], 240);
        assert_eq!(first["offset"], 0);
        assert_eq!(first["count"], 240);
        block.at = origin + Duration::from_millis(10);
        block.gap = true;
        block.pcm.fill(0.2);
        assert_eq!(wave.block(&block, origin).unwrap()["offset"], 480);
        block.at = origin + Duration::from_millis(19);
        block.gap = false;
        assert_eq!(wave.block(&block, origin).unwrap()["offset"], 960);
        block.at = origin + Duration::from_secs(70);
        block.gap = true;
        assert_eq!(wave.block(&block, origin).unwrap()["count"], 0);
        assert_eq!(wave.samples, MAX_SAMPLES);
        wave.writer.finalize().unwrap();
        let pcm = samples(&parent.join("test.wav"));
        assert_eq!(&pcm[..240], &[0.1; 240]);
        assert_eq!(&pcm[240..480], &[0.0; 240]);
        assert_eq!(&pcm[480..1440], &[0.2; 960]);
        std::fs::remove_dir_all(parent).unwrap();
    }
    fn stalled(origin: Instant) -> (Recorder, mpsc::Receiver<Event>) {
        let state = Arc::new(State {
            phase: AtomicU8::new(0),
            end_samples: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            error: Mutex::new(None),
            directory: directory(),
            origin,
        });
        let (tx, rx) = mpsc::sync_channel(1);
        (
            Recorder {
                active: Some(Active {
                    tx,
                    state: state.clone(),
                }),
                last: Some(state),
            },
            rx,
        )
    }
    #[test]
    fn full_queue_stops_recording_without_blocking_and_marks_incomplete() {
        let (mut recorder, _rx) = stalled(Instant::now());
        recorder.block(
            Track::Microphone,
            Instant::now(),
            &[0.2; 480],
            false.into(),
            0,
            None,
        );
        recorder.block(
            Track::Microphone,
            Instant::now(),
            &[0.2; 480],
            false.into(),
            0,
            None,
        );
        assert!(recorder.active.is_none());
        let status = recorder.status().unwrap();
        assert_eq!(status.dropped_blocks, 1);
        assert!(status.error.is_some());
    }
    #[test]
    fn time_limit_drops_sender_even_without_audio_blocks() {
        let (mut recorder, rx) = stalled(Instant::now() - Duration::from_secs(61));
        recorder.poll();
        assert!(recorder.active.is_none());
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(recorder.status().unwrap().elapsed_ms, 60_000);
    }
    #[test]
    fn writer_failure_is_reported_and_does_not_require_stopping_voice() {
        let parent = directory();
        std::fs::write(&parent, b"not a directory").unwrap();
        let mut recorder = Recorder::default();
        recorder
            .start(parent.clone(), AudioSettings::default(), json!({}))
            .unwrap();
        done(recorder.last.as_ref().unwrap());
        recorder.poll();
        let status = recorder.status().unwrap();
        assert!(status.error.is_some() && !status.active && !status.saving);
        assert!(recorder.active.is_none());
        std::fs::remove_file(parent).unwrap();
    }
    #[test]
    fn dropping_recorder_finalizes_files_and_repeated_start_cannot_replace_active_capture() {
        let parent = directory();
        let mut recorder = Recorder::default();
        recorder
            .start(parent.clone(), AudioSettings::default(), json!({}))
            .unwrap();
        assert!(
            recorder
                .start(parent.clone(), AudioSettings::default(), json!({}))
                .is_err()
        );
        let state = recorder.last.as_ref().unwrap().clone();
        drop(recorder);
        done(&state);
        assert!(
            std::fs::read_to_string(state.directory.join("timeline.jsonl"))
                .unwrap()
                .contains("\"complete\":true")
        );
        for name in FILES {
            let _ = samples(&state.directory.join(name));
        }
        std::fs::remove_dir_all(parent).unwrap();
    }
}
