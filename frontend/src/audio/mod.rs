mod format;
mod frames;
mod health;
pub mod jitter;
pub mod mixer;
pub mod processing;
pub mod transport;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use format::config;
use health::StreamHealth;
use mixer::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use thiscord_shared::audio::*;

pub fn devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();
    let mut result = vec![];
    for input in [true, false] {
        let list: Vec<_> = if input {
            host.input_devices().map_err(|e| e.to_string())?.collect()
        } else {
            host.output_devices().map_err(|e| e.to_string())?.collect()
        };
        for device in list {
            if let (Ok(id), Ok(description)) = (device.id(), device.description()) {
                result.push(AudioDevice {
                    id: id.to_string(),
                    name: description.name().to_owned(),
                    input,
                });
            }
        }
    }
    Ok(result)
}
fn device(id: Option<&str>, input: bool) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(id) = id {
        host.devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.id().is_ok_and(|v| v.to_string() == id))
    } else if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
    .ok_or_else(|| {
        "Selected audio device is unavailable. Refresh devices and select another.".into()
    })
}
// Dispatch to the actual PCM storage type. In particular, never reinterpret
// 24/32-bit audio as u16. Conversion is done in the existing bounded callbacks.
macro_rules! pcm_stream {
    ($sample:expr, $function:ident($($arg:expr),* $(,)?)) => {
        match $sample {
            cpal::SampleFormat::I8 => $function::<i8>($($arg),*),
            cpal::SampleFormat::I16 => $function::<i16>($($arg),*),
            cpal::SampleFormat::I24 => $function::<cpal::I24>($($arg),*),
            cpal::SampleFormat::I32 => $function::<i32>($($arg),*),
            cpal::SampleFormat::I64 => $function::<i64>($($arg),*),
            cpal::SampleFormat::U8 => $function::<u8>($($arg),*),
            cpal::SampleFormat::U16 => $function::<u16>($($arg),*),
            cpal::SampleFormat::U24 => $function::<cpal::U24>($($arg),*),
            cpal::SampleFormat::U32 => $function::<u32>($($arg),*),
            cpal::SampleFormat::U64 => $function::<u64>($($arg),*),
            cpal::SampleFormat::F32 => $function::<f32>($($arg),*),
            cpal::SampleFormat::F64 => $function::<f64>($($arg),*),
            _ => Err("Unsupported non-PCM audio sample format".into()),
        }
    };
}
fn output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut mixer: Mixer,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], info| {
                mixer.reference_time(frames::playback_time(Instant::now(), info.timestamp()));
                mixer.render(data, channels);
            },
            move |error| {
                health.report(false, error.kind());
            },
            None,
        )
        .map_err(|e| format!("Cannot open output device: {e}"))
}
fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut writer: frames::Writer,
    control: Arc<Controls>,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    device
        .build_input_stream(
            *config,
            move |data: &[T], info| {
                writer.begin(frames::capture_time(Instant::now(), info.timestamp()));
                let mut peak = 0.0_f32;
                let mut dropped = 0;
                for frame in data.chunks_exact(channels) {
                    let mono = capture_mono(frame);
                    peak = peak.max(mono.abs());
                    dropped += writer.push(mono);
                }
                control.peak.store(peak.to_bits(), Ordering::Relaxed);
                control.dropped.fetch_add(dropped, Ordering::Relaxed);
            },
            move |error| {
                health.report(true, error.kind());
            },
            None,
        )
        .map_err(|e| {
            format!("Cannot open microphone. Check OS microphone permission and device access: {e}")
        })
}

fn capture_mono<T: cpal::Sample>(frame: &[T]) -> f32
where
    f32: cpal::FromSample<T>,
{
    let mono = frame
        .iter()
        .map(|v| cpal::Sample::to_sample::<f32>(*v))
        .sum::<f32>()
        / frame.len() as f32;
    if mono.is_finite() {
        mono.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

pub enum Command {
    VoiceStart {
        settings: AudioSettings,
        outgoing: tokio::sync::mpsc::Sender<bytes::Bytes>,
        microphone: bool,
    },
    Packet {
        slot: usize,
        sequence: u16,
        payload: bytes::Bytes,
    },
    Roster(Vec<thiscord_shared::voice::Participant>),
    Start {
        settings: AudioSettings,
        microphone: bool,
    },
    Stop,
    Settings(AudioSettings),
    Volume {
        stream: usize,
        gain: f32,
    },
    Pressed(bool),
    Status,
    Peek,
}
#[derive(Clone)]
pub struct AudioEngine {
    sender: mpsc::SyncSender<Request>,
    stop: Arc<AtomicBool>,
    pressed: Arc<AtomicBool>,
    inhibit: Arc<AtomicBool>,
}
impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioEngine {
    pub fn notify(&self, command: Command) {
        self.urgent(&command);
        let _ = self.sender.try_send((command, None));
    }
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let pressed = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_pressed = pressed.clone();
        let inhibit = Arc::new(AtomicBool::new(false));
        let worker_inhibit = inhibit.clone();
        thread::Builder::new()
            .name("thiscord-audio-control".into())
            .spawn(move || run(receiver, worker_stop, worker_pressed, worker_inhibit))
            .expect("audio worker");
        Self {
            sender,
            stop,
            pressed,
            inhibit,
        }
    }
    fn urgent(&self, command: &Command) {
        match command {
            Command::Stop => {
                self.stop.store(true, Ordering::Release);
            }
            Command::Pressed(value) => {
                self.pressed.store(*value, Ordering::Release);
            }
            Command::Settings(settings)
            | Command::Start { settings, .. }
            | Command::VoiceStart { settings, .. } => {
                self.inhibit
                    .store(settings.muted || settings.deafened, Ordering::Release);
            }
            _ => {}
        }
    }
    pub fn command(&self, command: Command) -> Result<AudioStatus, String> {
        self.urgent(&command);
        let (tx, rx) = mpsc::channel();
        self.sender
            .try_send((command, Some(tx)))
            .map_err(|_| "Audio worker busy or stopped")?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Audio device operation timed out")?
    }
}
struct Remote {
    id: thiscord_shared::AccountId,
    label: String,
    jitter: jitter::Jitter,
    decoder: opus::Decoder,
}
struct Session {
    _output: cpal::Stream,
    _input: Option<cpal::Stream>,
    capture: frames::Reader,
    writers: Vec<StreamWriter>,
    control: Arc<Controls>,
    health: Arc<StreamHealth>,
    encoder: opus::Encoder,
    decoder: opus::Decoder,
    microphone: bool,
    phase: f32,
    next: Instant,
    started: Instant,
    hold: usize,
    outgoing: Option<tokio::sync::mpsc::Sender<bytes::Bytes>>,
    remotes: Vec<Option<Remote>>,
    playback: Instant,
    reference: frames::Reader,
    processed_level: f32,
    processing: processing::Processing,
}
impl Session {
    fn start(settings: &AudioSettings, microphone: bool) -> Result<Self, String> {
        settings.validate()?;
        let control = Arc::new(Controls::default());
        let health = Arc::new(StreamHealth::default());
        apply(&control, settings);
        let (writers, mixer) = mixer(control.clone());
        let (reference_writer, reference) = frames::queue();
        let mixer = mixer.with_reference(reference_writer);
        let output_device = device(settings.output.as_deref(), false)?;
        let output_config = config(&output_device, false)?;
        let output = pcm_stream!(
            output_config.sample_format(),
            output(
                &output_device,
                &output_config.config(),
                mixer,
                health.clone()
            )
        )?;
        let (producer, capture) = frames::queue();
        let input = if microphone {
            let d = device(settings.input.as_deref(), true)?;
            let c = config(&d, true)?;
            Some(pcm_stream!(
                c.sample_format(),
                input(&d, &c.config(), producer, control.clone(), health.clone())
            )?)
        } else {
            None
        };
        let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| e.to_string())?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(32_000))
            .map_err(|e| e.to_string())?;
        let decoder = opus::Decoder::new(RATE, opus::Channels::Mono).map_err(|e| e.to_string())?;
        writers[0].control.active.store(true, Ordering::Release);
        if !microphone {
            writers[1].control.active.store(true, Ordering::Release);
        }
        // Build the DSP before starting devices so startup cannot overflow queues.
        let processing = processing::Processing::new(settings)?;
        output.play().map_err(|e| e.to_string())?;
        if let Some(input) = &input {
            input.play().map_err(|e| e.to_string())?;
        }
        Ok(Self {
            _output: output,
            _input: input,
            capture,
            writers,
            control,
            health,
            encoder,
            decoder,
            microphone,
            phase: 0.0,
            next: Instant::now(),
            started: Instant::now(),
            hold: 0,
            outgoing: None,
            remotes: (0..MAX_STREAMS).map(|_| None).collect(),
            playback: Instant::now(),
            reference,
            processed_level: 0.0,
            processing,
        })
    }
    fn tick(
        &mut self,
        stop: &AtomicBool,
        pressed: &AtomicBool,
        inhibit: &AtomicBool,
    ) -> Result<(), String> {
        self.health.check()?;
        self.processing.render_queued(&mut self.reference)?;
        if self.outgoing.is_some() && Instant::now() >= self.playback {
            self.playback += Duration::from_millis(20);
            if self.playback.elapsed() > Duration::from_millis(100) {
                self.playback = Instant::now();
            }
            for (remote, writer) in self.remotes.iter_mut().zip(self.writers.iter_mut()) {
                if let Some(remote) = remote
                    && let Some(packet) = remote.jitter.pop()
                {
                    let mut pcm = [0.0_f32; FRAME];
                    if let Ok(n) = remote.decoder.decode_float(
                        packet.as_deref().unwrap_or(&[]),
                        &mut pcm,
                        false,
                    ) && n == FRAME
                    {
                        writer.write(&pcm[..n]);
                    }
                }
            }
        }
        if !self.microphone {
            if self.outgoing.is_some() {
                return Ok(());
            }
            if Instant::now() < self.next {
                return Ok(());
            }
            self.next = Instant::now() + Duration::from_millis(20);
            let mut low = [0.0; FRAME];
            let mut high = [0.0; FRAME];
            for i in 0..FRAME {
                low[i] = (self.phase * std::f32::consts::TAU * 440.0 / RATE as f32).sin() * 0.06;
                high[i] = (self.phase * std::f32::consts::TAU * 660.0 / RATE as f32).sin() * 0.06;
                self.phase = (self.phase + 1.0) % RATE as f32;
            }
            self.writers[0].write(&low);
            self.writers[1].write(&high);
            return Ok(());
        }
        // Drain complete 20 ms packets, including large device callbacks. The
        // ring itself bounds latency; trimming each callback to 60 ms destroys
        // capture continuity and prevents the echo filter from converging.
        for _ in 0..CAPACITY / FRAME {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let Some(pcm) = self.processing.capture_queued(&mut self.capture)? else {
                break;
            };
            if stop.load(Ordering::Acquire) {
                break;
            }
            self.control
                .pressed
                .store(pressed.load(Ordering::Acquire), Ordering::Relaxed);
            self.capture_packet(pcm, inhibit)?;
        }
        Ok(())
    }
    fn capture_packet(
        &mut self,
        mut pcm: [f32; FRAME],
        inhibit: &AtomicBool,
    ) -> Result<(), String> {
        let peak = pcm.iter().fold(0.0_f32, |p, v| p.max(v.abs()));
        self.processed_level = peak;
        if peak >= f32::from_bits(self.control.threshold.load(Ordering::Relaxed)) {
            self.hold = 10;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        let active = !inhibit.load(Ordering::Acquire)
            && !self.control.mute.load(Ordering::Relaxed)
            && !self.control.deafen.load(Ordering::Relaxed)
            && if self.control.push_to_talk.load(Ordering::Relaxed) {
                self.control.pressed.load(Ordering::Relaxed)
            } else {
                self.hold > 0
            };
        self.control.transmitting.store(active, Ordering::Relaxed);
        if !active {
            pcm.fill(0.0);
        }
        let mut packet = [0_u8; 4000];
        let n = self
            .encoder
            .encode_float(&pcm, &mut packet)
            .map_err(|e| e.to_string())?;
        if let Some(outgoing) = &self.outgoing {
            let _ = outgoing.try_send(bytes::Bytes::copy_from_slice(&packet[..n]));
            return Ok(());
        }
        let mut decoded = [0.0; FRAME];
        let n = self
            .decoder
            .decode_float(&packet[..n], &mut decoded, false)
            .map_err(|e| e.to_string())?;
        let written = self.writers[0].write(&decoded[..n]);
        self.control
            .dropped
            .fetch_add((n - written) as u64, Ordering::Relaxed);
        Ok(())
    }
    fn status(&self) -> AudioStatus {
        AudioStatus {
            running: true,
            input_level: self.processed_level,
            raw_input_level: f32::from_bits(self.control.peak.load(Ordering::Relaxed)),
            processing_resets: self.processing.resets,
            echo: self.processing.echo_diagnostics(),
            transmitting: self.control.transmitting.load(Ordering::Relaxed),
            message: if self.outgoing.is_some() {
                "Voice audio active"
            } else if self.microphone {
                "Microphone test: local Opus loopback (use headphones)"
            } else {
                "Output test: two independent streams"
            }
            .into(),
            streams: if self.outgoing.is_some() {
                self.remotes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        r.as_ref().map(|r| StreamLevel {
                            id: i.to_string(),
                            label: r.label.clone(),
                            volume: f32::from_bits(
                                self.writers[i].control.volume.load(Ordering::Relaxed),
                            ),
                        })
                    })
                    .collect()
            } else {
                (0..if self.microphone { 1 } else { 2 })
                    .map(|i| StreamLevel {
                        id: i.to_string(),
                        label: if self.microphone {
                            "Microphone loopback".into()
                        } else {
                            format!("Test stream {}", i + 1)
                        },
                        volume: f32::from_bits(
                            self.writers[i].control.volume.load(Ordering::Relaxed),
                        ),
                    })
                    .collect()
            },
            dropped_samples: self.control.dropped.load(Ordering::Relaxed)
                + self.control.reference_dropped.load(Ordering::Relaxed),
            underrun_samples: self.control.underruns.load(Ordering::Relaxed),
        }
    }
}
fn apply(c: &Controls, s: &AudioSettings) {
    c.mute.store(s.muted, Ordering::Relaxed);
    c.deafen.store(s.deafened, Ordering::Relaxed);
    c.master.store(s.output_volume.to_bits(), Ordering::Relaxed);
    c.threshold
        .store(s.activation_threshold.to_bits(), Ordering::Relaxed);
    c.push_to_talk
        .store(s.mode == TransmitMode::PushToTalk, Ordering::Relaxed);
}
fn stopped(message: String) -> AudioStatus {
    AudioStatus {
        running: false,
        input_level: 0.0,
        raw_input_level: 0.0,
        processing_resets: 0,
        echo: None,
        transmitting: false,
        message,
        streams: vec![],
        dropped_samples: 0,
        underrun_samples: 0,
    }
}
type Request = (Command, Option<mpsc::Sender<Result<AudioStatus, String>>>);
fn run(
    receiver: mpsc::Receiver<Request>,
    stop: Arc<AtomicBool>,
    pressed: Arc<AtomicBool>,
    inhibit: Arc<AtomicBool>,
) {
    let mut session: Option<Session> = None;
    let mut message = String::new();
    let mut lease = Instant::now();
    let mut previous = Instant::now();
    loop {
        // Release requests and PTT key-up cannot be lost behind a full media queue.
        if stop.swap(false, Ordering::AcqRel) {
            session = None;
            pressed.store(false, Ordering::Release);
            message = "Audio stopped; devices released".into();
        }
        if previous.elapsed() > Duration::from_secs(2) || lease.elapsed() > Duration::from_secs(3) {
            session = None;
            message = "Audio stopped after inactivity or suspend".into();
        }
        previous = Instant::now();
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok((command, reply)) => {
                if !matches!(
                    &command,
                    Command::Peek | Command::Packet { .. } | Command::Roster(_)
                ) {
                    lease = Instant::now();
                }
                let result = (|| -> Result<(), String> {
                    match command {
                        Command::Start {
                            settings,
                            microphone,
                        } => {
                            session = None;
                            session = Some(Session::start(&settings, microphone)?);
                            message.clear();
                        }
                        Command::VoiceStart {
                            settings,
                            outgoing,
                            microphone,
                        } => {
                            session = None;
                            let mut s = Session::start(&settings, microphone)?;
                            for writer in &s.writers {
                                writer.control.active.store(false, Ordering::Release);
                            }
                            s.outgoing = Some(outgoing);
                            session = Some(s);
                        }
                        Command::Packet {
                            slot,
                            sequence,
                            payload,
                        } => {
                            if let Some(s) = &mut session
                                && let Some(Some(r)) = s.remotes.get_mut(slot)
                            {
                                r.jitter.push(sequence, payload);
                            }
                        }
                        Command::Roster(members) => {
                            if let Some(s) = &mut session {
                                for slot in 0..MAX_STREAMS {
                                    let member = members.iter().find(|m| m.slot == slot);
                                    if s.remotes[slot].as_ref().map(|r| r.id)
                                        != member.map(|m| m.account_id)
                                    {
                                        s.writers[slot]
                                            .control
                                            .active
                                            .store(false, Ordering::Release);
                                        s.writers[slot]
                                            .control
                                            .generation
                                            .fetch_add(1, Ordering::AcqRel);
                                        s.writers[slot].volume(1.0)?;
                                        s.remotes[slot] = member
                                            .map(|m| {
                                                Ok::<_, String>(Remote {
                                                    id: m.account_id,
                                                    label: m.username.clone(),
                                                    jitter: Default::default(),
                                                    decoder: opus::Decoder::new(
                                                        RATE,
                                                        opus::Channels::Mono,
                                                    )
                                                    .map_err(|e| e.to_string())?,
                                                })
                                            })
                                            .transpose()?;
                                        s.writers[slot]
                                            .control
                                            .active
                                            .store(member.is_some(), Ordering::Release);
                                    }
                                }
                            }
                        }
                        Command::Stop => {
                            session = None;
                            message = "Audio stopped; devices released".into();
                        }
                        Command::Settings(settings) => {
                            settings.validate()?;
                            if let Some(s) = &mut session {
                                s.processing.settings(&settings)?;
                                if settings.deafened {
                                    for remote in s.remotes.iter_mut().flatten() {
                                        remote.jitter = Default::default();
                                    }
                                }
                                apply(&s.control, &settings);
                            }
                        }
                        Command::Volume { stream, gain } => {
                            let s = session.as_mut().ok_or("Audio is stopped")?;
                            s.writers
                                .get(stream)
                                .ok_or("Unknown audio stream")?
                                .volume(gain)?;
                        }
                        Command::Pressed(_) => {}
                        Command::Status | Command::Peek => {}
                    }
                    Ok(())
                })();
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| {
                        session
                            .as_ref()
                            .map(|s| s.status())
                            .unwrap_or_else(|| stopped(message.clone()))
                    }));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(s) = &mut session {
            s.control
                .pressed
                .store(pressed.load(Ordering::Acquire), Ordering::Relaxed);
            if s.outgoing.is_none()
                && s.started.elapsed() > Duration::from_secs(if s.microphone { 60 } else { 5 })
            {
                session = None;
                message = "Test finished; devices released".into();
            } else if let Err(error) = s.tick(&stop, &pressed, &inhibit) {
                session = None;
                message = error;
            }
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn integer_microphone_samples_convert_to_normalized_mono() {
        assert_eq!(capture_mono(&[cpal::I24::new(4194304).unwrap()]), 0.5);
        assert_eq!(capture_mono(&[1073741824_i32, -536870912_i32]), 0.125);
        assert_eq!(capture_mono(&[32768_u16, 32768]), 0.0);
        assert_eq!(capture_mono(&[f32::NAN]), 0.0);
    }
    #[test]
    fn saturated_media_queue_cannot_lose_stop_or_ptt_release() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let engine = AudioEngine {
            sender,
            stop: Arc::new(AtomicBool::new(false)),
            pressed: Arc::new(AtomicBool::new(true)),
            inhibit: Arc::new(AtomicBool::new(false)),
        };
        engine.notify(Command::Peek);
        engine.notify(Command::Pressed(false));
        engine.notify(Command::Stop);
        engine.notify(Command::Settings(AudioSettings {
            muted: true,
            ..Default::default()
        }));
        assert!(!engine.pressed.load(Ordering::Acquire));
        assert!(engine.stop.load(Ordering::Acquire));
        assert!(engine.inhibit.load(Ordering::Acquire));
    }
}
