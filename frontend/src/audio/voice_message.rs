//! Voice messages on dedicated CPAL streams, independent of the call engine so
//! they work in and out of calls. Device callbacks only move samples through
//! preallocated SPSC rings and read atomics; resampling for capture, Opus
//! encoding/decoding, Ogg (de)muxing and time-stretching run on worker
//! threads. PCM never leaves this process; only levels/positions are reported.
use super::*;
use crate::voice_clip::Stretcher;
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::*};
use std::sync::atomic::{AtomicU8, AtomicU32};
use thiscord_shared::voice_message::{
    MAX_VOICE_MESSAGE_MS, MIN_VOICE_MESSAGE_MS, OggOpus, OggOpusWriter, waveform,
};

/// Single input or output stream handed to the lifecycle reaper.
struct Single(cpal::Stream);
impl DeviceStreams for Single {
    fn play(&self) -> Result<(), String> {
        self.0.play().map_err(|e| e.to_string())
    }
}

fn store_peak(peak: &AtomicU32, value: f32) {
    // Non-negative finite floats order like their bit patterns.
    peak.fetch_max(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

/// Live recorder progress for the UI (about 20 per second).
#[derive(Clone, Copy)]
pub struct Level {
    pub elapsed_ms: u32,
    pub level: f32,
}

/// A finished recording kept in memory until it is sent or discarded.
pub struct Recording {
    pub ogg: Vec<u8>,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
}

/// How a recording ended.
pub enum Outcome {
    Ready(Recording),
    /// Stopped by reaching [`MAX_VOICE_MESSAGE_MS`].
    Limit(Recording),
    TooShort,
    Cancelled,
    Failed(String),
}

const RUNNING: u8 = 0;
const FINISH: u8 = 1;
const CANCEL: u8 = 2;

pub struct Recorder {
    stop: Arc<AtomicU8>,
}
impl Recorder {
    /// Opens the selected microphone (`None` = system default) and starts
    /// recording. `done` runs once on the worker with the outcome.
    pub fn start(
        input: Option<String>,
        gain: f32,
        mut levels: impl FnMut(Level) + Send + 'static,
        done: impl FnOnce(Outcome) + Send + 'static,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicU8::new(RUNNING));
        let flag = stop.clone();
        let (ready, opened) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("thiscord-voice-message-recorder".into())
            .spawn(move || {
                let outcome = record(input, gain, &flag, &ready, &mut levels);
                let _ = ready.try_send(Err("Microphone stopped".into()));
                done(outcome);
            })
            .map_err(|_| "Cannot start the recorder")?;
        match opened.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(Self { stop }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stop.store(CANCEL, Ordering::Release);
                Err("The microphone did not start in time".into())
            }
        }
    }
    pub fn finish(&self) {
        let _ = self
            .stop
            .compare_exchange(RUNNING, FINISH, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn cancel(&self) {
        self.stop.store(CANCEL, Ordering::Release);
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        // A dropped handle never leaves the microphone open.
        let _ = self
            .stop
            .compare_exchange(RUNNING, CANCEL, Ordering::AcqRel, Ordering::Acquire);
    }
}

fn capture<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut samples: HeapProd<f32>,
    peak: Arc<AtomicU32>,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let activity = health.clone();
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                if activity.disabled() {
                    return;
                }
                activity.callback(true);
                let mut high = 0f32;
                for frame in data.chunks_exact(channels) {
                    let mono = capture_mono(frame);
                    high = high.max(mono.abs());
                    // A full ring drops samples rather than blocking or allocating.
                    let _ = samples.try_push(mono);
                }
                store_peak(&peak, high);
            },
            move |error| health.report(true, error.kind()),
            None,
        )
        .map_err(|e| {
            format!("Cannot open microphone. Check OS microphone permission and device access: {e}")
        })
}

fn record(
    input: Option<String>,
    gain: f32,
    stop: &AtomicU8,
    ready: &mpsc::SyncSender<Result<(), String>>,
    levels: &mut dyn FnMut(Level),
) -> Outcome {
    let opened = (|| {
        let permit = lifecycle::acquire()?;
        let selected = selection::device(input.as_deref(), true)?;
        let supported = config(&selected.device, true)?;
        let stream_config = supported.config();
        let rate = stream_config.sample_rate;
        let (samples, consumer) = HeapRb::<f32>::new(rate as usize * 2).split();
        let peak = Arc::new(AtomicU32::new(0));
        let health = Arc::new(StreamHealth::default());
        let stream = pcm_stream!(
            supported.sample_format(),
            capture(
                &selected.device,
                &stream_config,
                samples,
                peak.clone(),
                health.clone()
            )
        )?;
        let managed = lifecycle::Managed::new(Single(stream), permit, health.clone());
        managed.play()?;
        Ok::<_, String>((managed, consumer, peak, health, rate))
    })();
    let (_managed, mut consumer, peak, health, rate) = match opened {
        Ok(opened) => {
            let _ = ready.try_send(Ok(()));
            opened
        }
        Err(error) => {
            let _ = ready.try_send(Err(error.clone()));
            return Outcome::Failed(error);
        }
    };
    let mut encoder = match opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
    {
        Ok(encoder) => encoder,
        Err(_) => return Outcome::Failed("Cannot start the Opus encoder".into()),
    };
    let _ = encoder.set_bitrate(opus::Bitrate::Bits(32_000));
    let _ = encoder.set_complexity(10);
    let _ = encoder.set_inband_fec(false);
    let _ = encoder.set_dtx(false);
    let pre_skip = encoder
        .get_lookahead()
        .ok()
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(312);
    let serial = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos() | 1);
    let mut writer = OggOpusWriter::new(serial, 1, pre_skip);
    let mut converter = (rate != RATE).then(|| device_rate::Converter::new(rate, RATE));
    let gain = if gain.is_finite() {
        gain.clamp(0.0, 4.0)
    } else {
        1.0
    };
    let mut frame = Vec::with_capacity(FRAME);
    let mut packet = vec![0u8; 1500];
    let mut frame_levels = Vec::with_capacity(MAX_VOICE_MESSAGE_MS as usize / 20 + 2);
    let mut encoded = 0u64;
    let limit = u64::from(MAX_VOICE_MESSAGE_MS) * u64::from(RATE / 1000);
    let mut last_report = Instant::now();
    let mut encode = |frame: &mut Vec<f32>, writer: &mut OggOpusWriter, levels: &mut Vec<f32>| {
        frame.resize(FRAME, 0.0);
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        levels.push(rms);
        let written = encoder.encode_float(frame, &mut packet).unwrap_or(0);
        if written > 0 {
            writer.push(&packet[..written], FRAME as u32);
        }
        frame.clear();
    };
    let mut limited = false;
    loop {
        let state = stop.load(Ordering::Acquire);
        if state == CANCEL {
            return Outcome::Cancelled;
        }
        while let Some(sample) = consumer.try_pop() {
            let sample = (sample * gain).clamp(-1.0, 1.0);
            let mut push = |value: f32| {
                if encoded < limit {
                    frame.push(value);
                    if frame.len() == FRAME {
                        encode(&mut frame, &mut writer, &mut frame_levels);
                        encoded += FRAME as u64;
                    }
                }
            };
            match &mut converter {
                Some(converter) => {
                    converter.push(sample);
                    while let Some(value) = converter.pop() {
                        push(value);
                    }
                }
                None => push(sample),
            }
        }
        if last_report.elapsed() >= Duration::from_millis(50) {
            last_report = Instant::now();
            let level = f32::from_bits(peak.swap(0, Ordering::Relaxed));
            levels(Level {
                elapsed_ms: (encoded / u64::from(RATE / 1000)) as u32,
                level,
            });
        }
        if encoded >= limit {
            limited = true;
            break;
        }
        if state == FINISH {
            break;
        }
        if let Err(error) = health.check() {
            return Outcome::Failed(error);
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !frame.is_empty() {
        encode(&mut frame, &mut writer, &mut frame_levels);
        encoded += FRAME as u64;
    }
    // One silent frame flushes the encoder lookahead covered by pre-skip.
    frame.clear();
    encode(&mut frame, &mut writer, &mut Vec::new());
    let duration_ms = (encoded / u64::from(RATE / 1000)) as u32;
    if duration_ms < MIN_VOICE_MESSAGE_MS {
        return Outcome::TooShort;
    }
    let recording = Recording {
        ogg: writer.finish(),
        duration_ms: duration_ms.min(MAX_VOICE_MESSAGE_MS),
        waveform: waveform(&frame_levels),
    };
    if limited {
        Outcome::Limit(recording)
    } else {
        Outcome::Ready(recording)
    }
}

/// A parsed clip shared between the cache and the player.
pub struct Clip {
    parsed: OggOpus,
    offsets: Vec<u64>,
}
impl Clip {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let parsed =
            thiscord_shared::voice_message::parse_ogg_opus(bytes).map_err(|e| e.0.to_owned())?;
        let offsets = parsed.offsets();
        Ok(Self { parsed, offsets })
    }
    pub fn duration_ms(&self) -> u32 {
        self.parsed.duration_ms
    }
}

#[derive(Clone, serde::Serialize)]
pub struct Status {
    pub key: String,
    /// "loading", "playing", "paused", "ended", "stopped" or "error".
    pub state: &'static str,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub speed: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub enum Command {
    Play {
        key: String,
        clip: Arc<Clip>,
        position_ms: u32,
        speed: f32,
        output: Option<String>,
        volume: f32,
    },
    Pause,
    Resume,
    Seek(u32),
    Speed(f32),
    Stop,
}

/// One clip plays at a time; starting another replaces it.
pub struct Player {
    commands: mpsc::Sender<Command>,
}
impl Player {
    pub fn new(events: impl FnMut(Status) + Send + 'static) -> Result<Self, String> {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("thiscord-voice-message-player".into())
            .spawn(move || play_loop(receiver, events))
            .map_err(|_| "Cannot start the voice message player")?;
        Ok(Self { commands })
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "Voice message player stopped".into())
    }
}

struct OutputControl {
    paused: AtomicBool,
    flush: AtomicBool,
    gain: AtomicU32,
}
struct Output {
    _managed: lifecycle::Managed,
    samples: HeapProd<f32>,
    control: Arc<OutputControl>,
    health: Arc<StreamHealth>,
    device: Option<String>,
}
/// About 100 ms of stretched audio stays queued ahead of the device.
const QUEUED: usize = 4_800;

fn render<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut samples: HeapCons<f32>,
    control: Arc<OutputControl>,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let activity = health.clone();
    let mut converter =
        (config.sample_rate != RATE).then(|| device_rate::Converter::new(RATE, config.sample_rate));
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                if activity.disabled() {
                    data.fill(T::from_sample(0.0));
                    return;
                }
                activity.callback(false);
                if control.flush.swap(false, Ordering::AcqRel) {
                    // Bounded by the ring capacity; no allocation.
                    while samples.try_pop().is_some() {}
                    if let Some(converter) = &mut converter {
                        converter.clear();
                    }
                }
                let paused = control.paused.load(Ordering::Acquire);
                let gain = f32::from_bits(control.gain.load(Ordering::Relaxed));
                for frame in data.chunks_exact_mut(channels) {
                    let value = if paused {
                        0.0
                    } else {
                        let next = match &mut converter {
                            None => samples.try_pop().unwrap_or(0.0),
                            Some(converter) => loop {
                                if let Some(value) = converter.pop() {
                                    break value;
                                }
                                match samples.try_pop() {
                                    Some(input) => converter.push(input),
                                    None => break 0.0,
                                }
                            },
                        };
                        (next * gain).clamp(-1.0, 1.0)
                    };
                    frame.fill(T::from_sample(value));
                }
            },
            move |error| health.report(false, error.kind()),
            None,
        )
        .map_err(|e| format!("Cannot open output device: {e}"))
}

fn open_output(device: Option<&str>, volume: f32) -> Result<Output, String> {
    let permit = lifecycle::acquire()?;
    let selected = selection::device(device, false)?;
    let supported = config(&selected.device, false)?;
    let stream_config = supported.config();
    let (samples, consumer) = HeapRb::<f32>::new(QUEUED * 4).split();
    let control = Arc::new(OutputControl {
        paused: AtomicBool::new(false),
        flush: AtomicBool::new(false),
        gain: AtomicU32::new(volume.clamp(0.0, 2.0).to_bits()),
    });
    let health = Arc::new(StreamHealth::default());
    let stream = pcm_stream!(
        supported.sample_format(),
        render(
            &selected.device,
            &stream_config,
            consumer,
            control.clone(),
            health.clone()
        )
    )?;
    let managed = lifecycle::Managed::new(Single(stream), permit, health.clone());
    managed.play()?;
    Ok(Output {
        _managed: managed,
        samples,
        control,
        health,
        device: device.map(str::to_owned),
    })
}

struct Active {
    key: String,
    clip: Arc<Clip>,
    decoder: opus::Decoder,
    stretcher: Stretcher,
    /// Next packet to decode.
    next: usize,
    /// Decoded samples still to drop (pre-skip and seek preroll).
    discard: u64,
    /// Playable-timeline samples handed to the stretcher.
    source: u64,
    paused: bool,
    draining: bool,
}
impl Active {
    fn speed(&self) -> f32 {
        self.stretcher.speed()
    }
    fn duration(&self) -> u32 {
        self.clip.duration_ms()
    }
    /// Position heard now: decoded minus what still waits in the output ring.
    fn position(&self, output: Option<&Output>) -> u32 {
        let queued =
            output.map_or(0, |o| o.samples.occupied_len()) as f64 * f64::from(self.speed());
        let heard = (self.source as f64 - queued).max(0.0);
        ((heard / f64::from(RATE / 1000)) as u32).min(self.duration())
    }
    fn seek(&mut self, milliseconds: u32) {
        let parsed = &self.clip.parsed;
        let target = u64::from(milliseconds.min(self.duration())) * u64::from(RATE / 1000);
        let stream = target + u64::from(parsed.pre_skip);
        let packet = self
            .clip
            .offsets
            .partition_point(|start| *start <= stream)
            .saturating_sub(1);
        // Four packets (80 ms for 20 ms frames) of preroll let the decoder converge.
        let start = packet.saturating_sub(4);
        self.next = start;
        self.discard = stream.saturating_sub(self.clip.offsets.get(start).copied().unwrap_or(0));
        self.source = target;
        self.draining = false;
        let _ = self.decoder.reset_state();
        let speed = self.speed();
        self.stretcher.reset(speed);
    }
}

fn status(active: &Active, output: Option<&Output>, state: &'static str) -> Status {
    Status {
        key: active.key.clone(),
        state,
        position_ms: active.position(output),
        duration_ms: active.duration(),
        speed: active.speed(),
        error: None,
    }
}

/// Waits briefly for the callback to drop queued audio after a seek.
fn flush(output: &Output) {
    output.control.flush.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_millis(100);
    while output.control.flush.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
}

fn play_loop(commands: mpsc::Receiver<Command>, mut events: impl FnMut(Status)) {
    let mut active: Option<Active> = None;
    let mut output: Option<Output> = None;
    let mut idle_since = Instant::now();
    let mut last_report = Instant::now();
    let mut pcm = vec![0f32; 5_760];
    let mut stretched = Vec::with_capacity(QUEUED * 2);
    loop {
        let playing = active.as_ref().is_some_and(|a| !a.paused);
        let wait = if playing {
            Duration::from_millis(10)
        } else {
            Duration::from_secs(1)
        };
        match commands.recv_timeout(wait) {
            Ok(Command::Play {
                key,
                clip,
                position_ms,
                speed,
                output: device,
                volume,
            }) => {
                if let Some(previous) = active.take()
                    && previous.key != key
                {
                    events(status(&previous, output.as_ref(), "stopped"));
                }
                if output
                    .as_ref()
                    .is_some_and(|o| o.device != device || o.health.check().is_err())
                {
                    output = None;
                }
                if output.is_none() {
                    match open_output(device.as_deref(), volume) {
                        Ok(opened) => output = Some(opened),
                        Err(error) => {
                            events(Status {
                                key,
                                state: "error",
                                position_ms,
                                duration_ms: clip.duration_ms(),
                                speed,
                                error: Some(error),
                            });
                            continue;
                        }
                    }
                }
                let Ok(decoder) = opus::Decoder::new(RATE, opus::Channels::Mono) else {
                    continue;
                };
                let output_ref = output.as_ref().unwrap();
                output_ref
                    .control
                    .gain
                    .store(volume.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
                flush(output_ref);
                output_ref.control.paused.store(false, Ordering::Release);
                let mut next = Active {
                    key,
                    clip,
                    decoder,
                    stretcher: Stretcher::new(speed),
                    next: 0,
                    discard: 0,
                    source: 0,
                    paused: false,
                    draining: false,
                };
                let start = if position_ms + 250 >= next.duration() {
                    0
                } else {
                    position_ms
                };
                next.seek(start);
                events(status(&next, output.as_ref(), "playing"));
                active = Some(next);
            }
            Ok(Command::Pause) => {
                if let Some(a) = active.as_mut() {
                    a.paused = true;
                    if let Some(o) = &output {
                        o.control.paused.store(true, Ordering::Release);
                    }
                    events(status(a, output.as_ref(), "paused"));
                }
            }
            Ok(Command::Resume) => {
                if let Some(a) = active.as_mut() {
                    a.paused = false;
                    if let Some(o) = &output {
                        o.control.paused.store(false, Ordering::Release);
                    }
                    events(status(a, output.as_ref(), "playing"));
                }
            }
            Ok(Command::Seek(milliseconds)) => {
                if let Some(a) = active.as_mut() {
                    if let Some(o) = &output {
                        flush(o);
                    }
                    a.seek(milliseconds);
                    let state = if a.paused { "paused" } else { "playing" };
                    events(status(a, output.as_ref(), state));
                }
            }
            Ok(Command::Speed(speed)) => {
                if let Some(a) = active.as_mut() {
                    let at = a.position(output.as_ref());
                    if let Some(o) = &output {
                        flush(o);
                    }
                    a.stretcher.reset(speed);
                    a.seek(at);
                    let state = if a.paused { "paused" } else { "playing" };
                    events(status(a, output.as_ref(), state));
                }
            }
            Ok(Command::Stop) => {
                if let Some(a) = active.take() {
                    if let Some(o) = &output {
                        flush(o);
                    }
                    events(status(&a, output.as_ref(), "stopped"));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let Some(a) = active.as_mut() else {
            // Release the device after a quiet period.
            if output.is_some() && idle_since.elapsed() > Duration::from_secs(10) {
                output = None;
            }
            continue;
        };
        idle_since = Instant::now();
        let Some(o) = output.as_mut() else {
            continue;
        };
        if let Err(error) = o.health.check() {
            let mut failed = status(a, Some(o), "error");
            failed.error = Some(error);
            events(failed);
            active = None;
            output = None;
            continue;
        }
        if a.paused {
            continue;
        }
        while o.samples.occupied_len() < QUEUED && !a.draining {
            let Some(packet) = a.clip.parsed.packets.get(a.next) else {
                stretched.clear();
                a.stretcher.finish(&mut stretched);
                o.samples.push_slice(&stretched);
                a.draining = true;
                break;
            };
            a.next += 1;
            let decoded = a.decoder.decode_float(packet, &mut pcm, false).unwrap_or(0);
            let skip = (a.discard as usize).min(decoded);
            a.discard -= skip as u64;
            let fresh = &pcm[skip..decoded];
            a.source += fresh.len() as u64;
            stretched.clear();
            a.stretcher.process(fresh, &mut stretched);
            o.samples.push_slice(&stretched);
        }
        let ended = a.draining && o.samples.is_empty();
        if ended {
            let mut done = status(a, Some(o), "ended");
            done.position_ms = done.duration_ms;
            events(done);
            active = None;
            continue;
        }
        if last_report.elapsed() >= Duration::from_millis(50) {
            last_report = Instant::now();
            events(status(a, Some(o), "playing"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_positions_respect_pre_skip_and_preroll() {
        let mut writer = OggOpusWriter::new(1, 1, 312);
        for _ in 0..100 {
            writer.push(&[0xF8, 0, 0], 960);
        }
        let clip = Arc::new(Clip::parse(&writer.finish()).unwrap());
        let mut active = Active {
            key: "k".into(),
            clip: clip.clone(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
            stretcher: Stretcher::new(1.0),
            next: 0,
            discard: 0,
            source: 0,
            paused: false,
            draining: false,
        };
        active.seek(0);
        assert_eq!((active.next, active.discard, active.source), (0, 312, 0));
        active.seek(1_000);
        // 1 s + 312 pre-skip lands in packet 50; decoding starts four earlier.
        assert_eq!(active.next, 46);
        assert_eq!(active.discard, 48_000 + 312 - 46 * 960);
        assert_eq!(active.position(None), 1_000);
        active.seek(u32::MAX);
        assert_eq!(active.position(None), clip.duration_ms());
    }
}
