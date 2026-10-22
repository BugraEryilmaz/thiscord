mod activity;
pub mod adaptive_fec;
mod codec;
pub mod connection;
mod cues;
mod device_rate;
mod device_users;
pub mod diagnostics;
mod dred;
pub mod failure;
#[cfg(test)]
#[path = "../../tests/support/audio_fixture.rs"]
#[allow(dead_code)]
mod fixture;
mod format;
mod frames;
mod gate;
mod health;
pub mod jitter;
mod lifecycle;
pub mod media_runtime;
pub mod mixer;
pub mod outgoing;
mod playout_rate;
#[cfg(test)]
mod playout_tests;
pub mod processing;
pub mod quality;
#[cfg(test)]
mod receive_tests;
pub mod red;
mod red_lifecycle;
use quality::{Count, Stage};
mod reconfigure;
pub mod reconnect;
mod recording;
mod recovery;
mod selection;
pub mod transmit;
pub mod transport;
pub mod volumes;
mod worker_health;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use format::config;
use health::StreamHealth;
use mixer::*;
use ringbuf::traits::Observer;
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
    let activity = health.clone();
    let mut converter = (config.sample_rate != RATE)
        .then(|| device_rate::Playback::new(config.sample_rate, &mut mixer));
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], info| {
                if activity.disabled() {
                    data.fill(T::from_sample(0.0));
                    return;
                }
                activity.callback(false);
                let now = Instant::now();
                let playback = frames::playback_time(now, info.timestamp());
                if let Some(converter) = &mut converter {
                    converter.render(&mut mixer, data, channels, playback, now);
                } else {
                    mixer.reference_time(playback);
                    mixer.render(data, channels);
                }
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
    let activity = health.clone();
    let mut capture_clock = frames::CaptureClock::default();
    let mut converter =
        (config.sample_rate != RATE).then(|| device_rate::Converter::new(config.sample_rate, RATE));
    device
        .build_input_stream(
            *config,
            move |data: &[T], info| {
                if activity.disabled() {
                    return;
                }
                activity.callback(true);
                let capture_at = capture_clock.map(Instant::now(), info.timestamp());
                if let Some(converter) = &mut converter {
                    writer.begin_capture(converter.capture_time(capture_at, data.len() / channels));
                } else {
                    writer.begin_capture(capture_at);
                }
                let mut peak = 0.0_f32;
                let mut dropped = 0;
                let mut clipped = 0;
                for frame in data.chunks_exact(channels) {
                    let mono = capture_mono(frame);
                    peak = peak.max(mono.abs());
                    clipped += u64::from(mono.abs() >= 0.999);
                    if let Some(converter) = &mut converter {
                        converter.push(mono);
                        while let Some(sample) = converter.pop() {
                            dropped += writer.push(sample);
                        }
                    } else {
                        dropped += writer.push(mono);
                    }
                }
                control
                    .counts
                    .input_samples
                    .add((data.len() / channels) as u64);
                control.counts.input_clipped.add(clipped);
                control.peak.store(peak.to_bits(), Ordering::Relaxed);
                control.counts.capture_overflow.add(dropped);
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
    Notification {
        kind: thiscord_shared::notifications::Kind,
        volume: u8,
    },
    DebugStart(std::path::PathBuf),
    DebugStop,
    VoiceStart {
        volumes: volumes::Profile,
        connection: connection::Connection,
        settings: AudioSettings,
        outgoing: tokio::sync::mpsc::Sender<outgoing::EncodedFrame>,
        microphone: bool,
    },
    Packet {
        payload_type: u8,
        source_epoch: Option<u32>,
        privacy_epoch: Option<u32>,
        marker: bool,
        connection: connection::Connection,
        slot: usize,
        sequence: u16,
        timestamp: u32,
        received_at: Instant,
        payload: bytes::Bytes,
    },
    Roster {
        connection: connection::Connection,
        members: Vec<thiscord_shared::voice::Participant>,
    },
    ReceiveBoundary {
        connection: connection::Connection,
        generation: u64,
        epoch: Option<u32>,
    },
    Start {
        settings: AudioSettings,
        microphone: bool,
    },
    Stop,
    Settings(AudioSettings),
    CurrentSettings(mpsc::Sender<Option<AudioSettings>>),
    Volume {
        target: Option<SpeakerVolumeTarget>,
        stream: usize,
        gain: f32,
    },
    Pressed(bool),
    Status,
    Peek,
    Keepalive(connection::Connection),
    #[cfg(test)]
    CrashWorker,
}
impl Command {
    fn operation(&self) -> u8 {
        use worker_health::*;
        match self {
            Self::Start { .. } | Self::VoiceStart { .. } => START,
            Self::Settings(_) => SETTINGS,
            Self::Stop => STOP,
            Self::Packet { .. } => MEDIA,
            Self::Status | Self::Peek | Self::Keepalive(_) => STATUS,
            _ => OTHER,
        }
    }
}
#[derive(Clone)]
pub struct AudioEngine {
    sender: mpsc::SyncSender<Request>,
    media: mpsc::SyncSender<Command>,
    probe: Arc<worker_health::Probe>,
    stop: Arc<AtomicBool>,
    pressed: Arc<AtomicBool>,
    inhibit: Arc<AtomicBool>,
    debug_stop: Arc<AtomicBool>,
    deafen: Arc<AtomicBool>,
}
impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioEngine {
    pub fn outgoing_control(&self) -> Arc<outgoing::Control> {
        self.probe.outgoing.clone()
    }
    pub fn diagnostic_snapshot(&self) {
        diagnostics::event("requested_worker_snapshot", self.probe.snapshot());
    }
    pub fn notify(&self, command: Command) {
        if matches!(
            command,
            Command::Roster { .. } | Command::ReceiveBoundary { .. }
        ) {
            let _ = self.notify_control(command);
            return;
        }
        self.urgent(&command);
        if matches!(command, Command::Packet { .. }) {
            if let Err(error) = self.media.try_send(command) {
                let (mpsc::TrySendError::Full(Command::Packet { connection, .. })
                | mpsc::TrySendError::Disconnected(Command::Packet { connection, .. })) = error
                else {
                    return;
                };
                connection
                    .metrics
                    .add(Count::IncomingQueueDroppedPackets, 1);
                self.probe.packet_drops.fetch_add(1, Ordering::Relaxed);
            }
        } else if self.sender.try_send((command, None)).is_err() {
            diagnostics::event("control_notification_dropped", self.probe.snapshot());
        }
    }
    /// Privacy-critical controls may not be silently dropped. The caller must
    /// reconnect after an error; close this command's connection immediately.
    pub fn notify_control(&self, command: Command) -> Result<(), String> {
        if !matches!(
            command,
            Command::Roster { .. } | Command::ReceiveBoundary { .. }
        ) {
            return Err("Not a voice privacy control".into());
        }
        self.urgent(&command);
        self.sender.try_send((command, None)).map_err(|error| {
            let request = match error {
                mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                    request
                }
            };
            self.probe.outgoing.pause_receive();
            self.probe.outgoing.enable_receive_protection();
            match request.0 {
                Command::Roster { connection, .. }
                | Command::ReceiveBoundary { connection, .. } => connection.close(),
                _ => unreachable!(),
            }
            "Voice privacy control could not be queued; reconnect voice".into()
        })
    }
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(32);
        let (media, packets) = mpsc::sync_channel(128);
        let probe = worker_health::Probe::new();
        let worker_probe = probe.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let pressed = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_pressed = pressed.clone();
        let inhibit = Arc::new(AtomicBool::new(false));
        let worker_inhibit = inhibit.clone();
        let debug_stop = Arc::new(AtomicBool::new(false));
        let worker_debug_stop = debug_stop.clone();
        let deafen = Arc::new(AtomicBool::new(false));
        let worker_deafen = deafen.clone();
        thread::Builder::new()
            .name("thiscord-audio-control".into())
            .spawn(move || {
                diagnostics::event("worker_started", worker_probe.snapshot());
                for restart in 0..=3 {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(
                        &receiver, &packets, worker_stop.clone(), worker_pressed.clone(),
                        worker_inhibit.clone(), worker_debug_stop.clone(), worker_deafen.clone(), &worker_probe,
                    )));
                    if result.is_ok() { break; }
                    worker_inhibit.store(true, Ordering::Release);
                    worker_probe.outgoing.invalidate();
                    worker_deafen.store(true, Ordering::Release);
                    worker_pressed.store(false, Ordering::Release);
                    worker_probe.active.store(false, Ordering::Release);
                    worker_probe.restarts.fetch_add(1, Ordering::Relaxed);
                    diagnostics::event("worker_panic_recovery", serde_json::json!({"restart":restart,"state":worker_probe.snapshot()}));
                    while let Ok((_, reply)) = receiver.try_recv() { if let Some(reply) = reply { let _ = reply.send(Err("Audio worker restarted after an internal failure. Join voice again; diagnostic logs were saved.".into())); } }
                    while packets.try_recv().is_ok() {}
                }
                worker_probe.exited();
            })
            .expect("audio worker");
        Self {
            sender,
            media,
            probe,
            stop,
            pressed,
            inhibit,
            debug_stop,
            deafen,
        }
    }
    fn urgent(&self, command: &Command) {
        match command {
            Command::VoiceStart { connection, .. } if !connection.active() => {}
            Command::ReceiveBoundary {
                connection,
                epoch: None,
                ..
            } if connection.active() => {
                self.probe.outgoing.pause_receive();
                self.probe.outgoing.enable_receive_protection();
            }
            Command::DebugStop => {
                self.debug_stop.store(true, Ordering::Release);
            }
            Command::Stop => {
                self.probe.outgoing.invalidate();
                self.stop.store(true, Ordering::Release);
            }
            Command::Pressed(value) => {
                if !value || !self.pressed.load(Ordering::Acquire) {
                    self.probe.outgoing.invalidate();
                }
                self.pressed.store(*value, Ordering::Release);
            }
            Command::Settings(settings)
            | Command::Start { settings, .. }
            | Command::VoiceStart { settings, .. } => {
                self.probe.outgoing.invalidate();
                self.inhibit
                    .store(settings.muted || settings.deafened, Ordering::Release);
                self.deafen.store(settings.deafened, Ordering::Release);
            }
            _ => {}
        }
    }
    /// Safety controls can tighten immediately while a settings save/preparation
    /// is in flight. Only an ordered settings command can relax them again.
    pub fn restrict(&self, settings: &AudioSettings) {
        if settings.muted || settings.deafened {
            self.probe.outgoing.invalidate();
            self.inhibit.store(true, Ordering::Release);
        }
        if settings.deafened {
            self.deafen.store(true, Ordering::Release);
        }
    }
    pub fn command(&self, command: Command) -> Result<AudioStatus, String> {
        self.urgent(&command);
        let (tx, rx) = mpsc::channel();
        self.sender
            .try_send((command, Some(tx)))
            .map_err(|error| {
                let (event, message) = match error {
                    mpsc::TrySendError::Full(_) => ("control_queue_full", "Audio control queue is full. Open diagnostic logs; the audio worker may be stalled."),
                    mpsc::TrySendError::Disconnected(_) => ("control_worker_disconnected", "Audio worker has exited. Open diagnostic logs, then restart Thiscord."),
                };
                diagnostics::event(event, self.probe.snapshot());
                message
            })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|error| {
                let (event, message) = match error {
                    mpsc::RecvTimeoutError::Timeout => ("control_response_timeout", "Audio worker did not respond within 10 seconds. Open diagnostic logs; the driver or processing worker may be stalled."),
                    mpsc::RecvTimeoutError::Disconnected => ("control_reply_disconnected", "Audio operation was interrupted by a worker failure. Join again; diagnostic logs were saved."),
                };
                diagnostics::event(event, self.probe.snapshot());
                message
            })?
    }
    pub fn current_settings(&self) -> Result<Option<AudioSettings>, String> {
        let (tx, rx) = mpsc::channel();
        self.command(Command::CurrentSettings(tx))?;
        rx.recv().map_err(|_| "Audio worker stopped".into())
    }
}
struct Remote {
    source_epoch: u32,
    privacy_epoch: u32,
    since: Instant,
    deafened: bool,
    decode_stats: codec::DecodeStats,
    activity: activity::Activity,
    id: thiscord_shared::AccountId,
    label: String,
    jitter: jitter::Jitter,
    decoder: opus::Decoder,
    rate: playout_rate::Rate,
}
impl Remote {
    fn set_deep_redundancy(
        &mut self,
        writer: &StreamWriter,
        enabled: bool,
        now: Instant,
    ) -> Result<(), String> {
        // Epoch-zero sources cannot fence redundant audio at remote boundaries.
        let enabled = enabled && self.source_epoch != 0 && self.privacy_epoch != 0;
        if self.jitter.deep_redundancy != enabled {
            self.clear(writer, now);
            if self.decode_stats.reset_failed {
                return Err("Opus full decoder reset failed".into());
            }
            self.jitter.set_deep_redundancy(enabled);
        }
        if let Err(error) = self.decoder.set_complexity(if enabled { 5 } else { 0 }) {
            self.decode_stats.reset_failed = true;
            self.deafened = true;
            return Err(error.to_string());
        }
        Ok(())
    }
    fn push(
        &mut self,
        sequence: u16,
        timestamp: u32,
        payload: bytes::Bytes,
        received_at: Instant,
        now: Instant,
    ) -> bool {
        if !self.deafened
            && received_at >= self.since
            && opus::packet::get_nb_samples(&payload, RATE).is_ok_and(|n| n == FRAME)
        {
            let no_data = payload.len() <= 2;
            let accepted = self
                .jitter
                .push(sequence, timestamp, payload, received_at, now);
            if accepted {
                self.jitter.packet_flags(timestamp, false, no_data);
            }
            accepted
        } else {
            self.jitter.quality.add(Count::ReceivedPackets, 1);
            if !self.deafened && received_at >= self.since {
                self.jitter.quality.add(Count::InvalidPackets, 1);
            }
            self.jitter.reject();
            false
        }
    }
    fn clear(&mut self, writer: &StreamWriter, now: Instant) {
        self.since = now;
        self.jitter.discard();
        self.reset_decoder(writer);
    }
    fn set_deafened(&mut self, writer: &StreamWriter, deafened: bool, now: Instant) {
        if deafened != self.deafened {
            self.deafened = deafened;
            self.clear(writer, now);
        }
    }
    fn reset_decoder(&mut self, writer: &StreamWriter) {
        if !codec::reset(&mut self.decoder, &mut self.decode_stats) {
            self.deafened = true;
        }
        self.activity = Default::default();
        self.rate.reset();
        writer.discard();
    }
    fn refill(&mut self, writer: &mut StreamWriter, now: Instant) {
        writer.control.counts.drain(&self.jitter.quality);
        self.jitter
            .output_callback(writer.callback_samples(), writer.callback_due(now));
        if self.deafened {
            return;
        }
        if self.jitter.prepare(now) {
            self.reset_decoder(writer);
            if self.deafened {
                return;
            }
        }
        let metrics = self.jitter.metrics();
        writer
            .control
            .pending_media
            .store(self.jitter.pending_media(), Ordering::Relaxed);
        writer
            .control
            .quiet
            .store(metrics.dtx || metrics.inactive, Ordering::Release);
        if !writer.ready() {
            return;
        }
        // Device-sized, bounded refill work. The callback remains PCM-only;
        // jitter buffering and Opus stay on this worker.
        for _ in 0..self.jitter.refill_budget() {
            let Some(scheduled) = self.jitter.poll(now, writer.queue.occupied_len()) else {
                break;
            };
            if scheduled.reset {
                if !codec::reset(&mut self.decoder, &mut self.decode_stats) {
                    self.deafened = true;
                    writer.discard();
                    return;
                }
                self.rate.reset();
            }
            let mut pcm = [0.0_f32; FRAME];
            let samples = match &scheduled.decode {
                jitter::Playout::Conceal(samples) => *samples,
                _ => FRAME,
            };
            let errors = self.decode_stats.decode_errors;
            let plc = self.decode_stats.plc_frames;
            let fec = self.decode_stats.fec_attempts;
            let timer = self.jitter.quality.timed(Stage::Decode);
            if codec::decode(
                &mut self.decoder,
                &scheduled.decode,
                &mut pcm,
                &mut self.decode_stats,
            ) {
                self.activity.observe(&pcm, now);
            }
            drop(timer);
            if self.decode_stats.reset_failed {
                self.deafened = true;
                writer.discard();
                return;
            }
            self.jitter
                .quality
                .add(Count::DecodedSamples, samples as u64);
            self.jitter.quality.add(
                Count::PlcSamples,
                (self.decode_stats.plc_frames - plc) * FRAME as u64,
            );
            self.jitter
                .quality
                .add(Count::FecAttempts, self.decode_stats.fec_attempts - fec);
            self.jitter.quality.add(
                Count::DecodeErrors,
                self.decode_stats.decode_errors - errors,
            );
            if self.decode_stats.decode_errors > errors {
                self.jitter.decode_failed();
            }
            let mut corrected = [0.0; playout_rate::MAX_OUTPUT];
            let count =
                self.rate
                    .process(&pcm[..samples], scheduled.correction_ppm, &mut corrected);
            let written = writer.write(&corrected[..count]);
            self.jitter
                .quality
                .add(Count::PcmOverflowSamples, (count - written) as u64);
        }
        let metrics = self.jitter.metrics();
        writer
            .control
            .pending_media
            .store(self.jitter.pending_media(), Ordering::Relaxed);
        writer
            .control
            .quiet
            .store(metrics.dtx || metrics.inactive, Ordering::Release);
    }
}
struct Devices {
    info: serde_json::Value,
    streams: Box<dyn DeviceStreams>,
    capture: frames::Reader,
    reference: frames::Reader,
    writers: Vec<StreamWriter>,
    control: Arc<Controls>,
    health: Arc<StreamHealth>,
}
trait DeviceStreams: Send {
    fn play(&self) -> Result<(), String>;
}
struct CpalStreams {
    output: cpal::Stream,
    input: Option<cpal::Stream>,
}
struct SilentStreams;
impl DeviceStreams for SilentStreams {
    fn play(&self) -> Result<(), String> {
        Ok(())
    }
}
impl DeviceStreams for CpalStreams {
    fn play(&self) -> Result<(), String> {
        self.output.play().map_err(|e| e.to_string())?;
        if let Some(input) = &self.input {
            input.play().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
impl Devices {
    fn silent(settings: &AudioSettings) -> Self {
        let control = Arc::new(Controls::default());
        apply(&control, settings);
        let (writers, _) = mixer(control.clone());
        Self {
            info: serde_json::Value::Null,
            streams: Box::new(SilentStreams),
            capture: frames::queue().1,
            reference: frames::queue().1,
            writers,
            control,
            health: Arc::new(StreamHealth::default()),
        }
    }
    fn prepare(settings: &AudioSettings, microphone: bool) -> Result<Self, String> {
        settings.validate()?;
        let permit = lifecycle::acquire()?;
        diagnostics::event(
            "device_open_begin",
            serde_json::json!({"microphone":microphone,"default_input":settings.input.is_none(),"default_output":settings.output.is_none()}),
        );
        let control = Arc::new(Controls::default());
        let health = Arc::new(StreamHealth::default());
        apply(&control, settings);
        let (writers, mixer) = mixer(control.clone());
        let (reference_writer, reference) = frames::queue();
        let mixer = mixer.with_reference(reference_writer);
        // Resolve both endpoints before allocating any streams. A stale saved ID
        // falls back independently, leaving the other selected device intact.
        let selected_output = selection::device(settings.output.as_deref(), false)?;
        let selected_input = microphone
            .then(|| selection::device(settings.input.as_deref(), true))
            .transpose()?;
        let output_device = selected_output.device;
        let output_name = output_device
            .description()
            .ok()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|| "selected output".into());
        let output_config = config(&output_device, false)?;
        diagnostics::event(
            "output_stream_open",
            serde_json::json!({"name":output_name,"rate":output_config.sample_rate(),"channels":output_config.channels(),"format":format!("{:?}",output_config.sample_format())}),
        );
        let output = pcm_stream!(
            output_config.sample_format(),
            output(
                &output_device,
                &output_config.config(),
                mixer,
                health.clone()
            )
        )
        .map_err(|e| format!("Output \"{output_name}\": {e}"))?;
        diagnostics::event("output_stream_prepared", serde_json::json!({}));
        let (producer, capture) = frames::queue();
        let mut input_info = serde_json::Value::Null;
        let input = if let Some(selected) = selected_input {
            let d = selected.device;
            let c = config(&d, true)?;
            input_info = serde_json::json!({"name":d.description().ok().map(|d|d.name().to_owned()),"fallback":selected.fallback,
                "channels":c.channels(),"sample_rate":c.sample_rate(),"format":format!("{:?}",c.sample_format())});
            diagnostics::event("input_stream_open", input_info.clone());
            let input_name = d
                .description()
                .ok()
                .map(|d| d.name().to_owned())
                .unwrap_or_else(|| "selected microphone".into());
            Some(
                pcm_stream!(
                    c.sample_format(),
                    input(&d, &c.config(), producer, control.clone(), health.clone())
                )
                .map_err(|e| format!("Microphone \"{input_name}\": {e}"))?,
            )
        } else {
            None
        };
        diagnostics::event(
            "device_open_complete",
            serde_json::json!({"microphone":microphone}),
        );
        Ok(Self {
            info: serde_json::json!({"input":input_info,"output":{
                "fallback":selected_output.fallback,
                "name":output_device.description().ok().map(|d|d.name().to_owned()),
                "channels":output_config.channels(),"sample_rate":output_config.sample_rate(),
                "format":format!("{:?}",output_config.sample_format())}}),
            streams: Box::new(lifecycle::Managed::new(
                CpalStreams { output, input },
                permit,
                health.clone(),
            )),
            capture,
            reference,
            writers,
            control,
            health,
        })
    }
    fn play(&self) -> Result<(), String> {
        self.streams.play()
    }
    fn inherit(&self, old: &Self) {
        old.control.deafen.store(true, Ordering::Release);
        if let Some(guard) = old.control.receive_guard.get() {
            let _ = self.control.receive_guard.set(guard.clone());
        }
        self.control.cues.store(
            old.control.cues.swap(0, Ordering::AcqRel),
            Ordering::Release,
        );
        for (next, previous) in self
            .control
            .cue_volumes
            .iter()
            .zip(&old.control.cue_volumes)
        {
            next.store(previous.load(Ordering::Acquire), Ordering::Release);
        }
        for (next, previous) in self.writers.iter().zip(&old.writers) {
            next.control.volume.store(
                previous.control.volume.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            next.control.active.store(
                previous.control.active.load(Ordering::Acquire),
                Ordering::Release,
            );
        }
    }
}
struct Session {
    device_watch: health::Watch,
    recovery: recovery::Recovery,
    volumes: Option<volumes::Profile>,
    connection: Option<connection::Connection>,
    settings: AudioSettings,
    devices: Devices,
    encoder: transmit::Encoder,
    decoder: opus::Decoder,
    microphone: bool,
    phase: f32,
    next: Instant,
    started: Instant,
    gate: gate::Gate,
    capture_epoch: u64,
    capture_after: Option<Instant>,
    record_boundary: Option<Instant>,
    reset_codec: bool,
    delivery: connection::Connection,
    outgoing: Option<tokio::sync::mpsc::Sender<outgoing::EncodedFrame>>,
    media_clock: outgoing::MediaClock,
    transmit_control: Arc<outgoing::Control>,
    encoder_generation: u64,
    encoder_active: bool,
    remotes: Vec<Option<Remote>>,
    receive_fences: [Option<red_lifecycle::ReceiveFence>; MAX_STREAMS],
    receive_floor: u32,
    processed_level: f32,
    processing: processing::Processing,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.delivery.close();
        self.transmit_control.cancel_pending();
        self.devices.health.disable();
        diagnostics::quality(|| self.quality());
        if let Some(connection) = &self.connection {
            connection.close();
        }
        let mut details = serde_json::json!({"voice":self.connection.is_some(),"health":self.devices.health.diagnostics(),"transmit":self.encoder.stats});
        if let Some(connection) = &self.connection {
            details = connection.diagnostic_details(details);
        }
        diagnostics::event("audio_session_ended", details);
    }
}
impl Session {
    fn clear_gate(&mut self) {
        self.gate.reset();
        self.delivery.close();
        self.transmit_control.cancel_pending();
        self.delivery = Default::default();
        self.reset_codec = true;
        self.devices
            .control
            .transmitting
            .store(false, Ordering::Release);
    }
    fn reset_gate(&mut self, at: Instant) {
        self.clear_gate();
        self.capture_after = Some(at);
        self.record_boundary = Some(at);
        self.processing.restrict_capture(at);
    }
    fn capture_boundary(&mut self, epoch: u64, at: Instant) {
        if self.capture_epoch != epoch {
            self.capture_epoch = epoch;
            self.reset_gate(at);
        }
    }
    fn resume_devices(
        &mut self,
        devices: Devices,
        stop: &AtomicBool,
        pressed: &AtomicBool,
        deafen: &AtomicBool,
    ) -> Result<bool, String> {
        if stop.load(Ordering::Acquire) || self.connection.as_ref().is_some_and(|c| !c.active()) {
            return Ok(false);
        }
        devices.inherit(&self.devices);
        apply(&devices.control, &self.settings);
        devices.control.deafen.store(
            self.settings.deafened || deafen.load(Ordering::Acquire),
            Ordering::Release,
        );
        devices
            .control
            .pressed
            .store(pressed.load(Ordering::Acquire), Ordering::Release);
        devices.play()?;
        self.recovery.succeeded(&devices.info);
        self.collect_metrics();
        self.transmit_control.invalidate();
        self.devices = devices;
        self.media_clock.restart_device();
        self.device_watch = Default::default();
        diagnostics::event("device_active", self.devices.info.clone());
        self.processing.reset();
        self.reset_gate(Instant::now());
        self.next = Instant::now();
        for (remote, writer) in self.remotes.iter_mut().zip(&self.devices.writers) {
            if let Some(remote) = remote {
                remote.clear(writer, Instant::now());
            }
        }
        Ok(true)
    }
    fn set_volume(
        &mut self,
        stream: usize,
        target: Option<SpeakerVolumeTarget>,
        gain: f32,
    ) -> Result<(), String> {
        if let Some(profile) = &mut self.volumes {
            let target = target.ok_or("Speaker identity is missing")?;
            if self
                .remotes
                .get(stream)
                .and_then(|r| r.as_ref())
                .map(|r| r.id)
                != Some(target.account_id)
            {
                return Err("Speaker changed; adjust the current speaker instead".into());
            }
            if target.shared_audio != (stream >= thiscord_shared::voice::ROOM_CAPACITY) {
                return Err("Audio stream kind changed".into());
            }
            profile.set(target, gain)?;
            self.devices.writers[stream].volume(gain)?;
            return Ok(());
        } else if target.is_some() {
            return Err("Voice connection changed".into());
        }
        self.devices
            .writers
            .get(stream)
            .ok_or("Unknown audio stream")?
            .volume(gain)?;
        Ok(())
    }

    fn start(settings: &AudioSettings, microphone: bool) -> Result<Self, String> {
        settings.validate()?;
        diagnostics::event(
            "processing_init_begin",
            serde_json::json!({"echo":settings.echo_cancellation,"neural_echo":settings.neural_echo,"suppression":settings.noise_suppression,"suppression_model":settings.noise_suppression_model}),
        );
        let mut processing = processing::Processing::new(settings)?;
        processing.prewarm()?;
        diagnostics::event("processing_init_complete", serde_json::json!({}));
        let recovery = recovery::Recovery::starting();
        let devices = Devices::silent(settings);
        let mut encoder = transmit::Encoder::new(true).map_err(|e| e.to_string())?;
        encoder
            .set_deep_redundancy(settings.deep_redundancy)
            .map_err(|e| e.to_string())?;
        let decoder = opus::Decoder::new(RATE, opus::Channels::Mono).map_err(|e| e.to_string())?;
        devices.writers[0]
            .control
            .active
            .store(true, Ordering::Release);
        if !microphone {
            devices.writers[1]
                .control
                .active
                .store(true, Ordering::Release);
        }
        Ok(Self {
            recovery,
            device_watch: Default::default(),
            settings: settings.clone(),
            volumes: None,
            connection: None,
            devices,
            encoder,
            decoder,
            microphone,
            phase: 0.0,
            next: Instant::now(),
            started: Instant::now(),
            gate: Default::default(),
            capture_epoch: 0,
            capture_after: None,
            record_boundary: None,
            reset_codec: false,
            delivery: Default::default(),
            outgoing: None,
            media_clock: Default::default(),
            transmit_control: Default::default(),
            encoder_generation: 0,
            encoder_active: false,
            remotes: (0..MAX_STREAMS).map(|_| None).collect(),
            receive_fences: Default::default(),
            receive_floor: 0,
            processed_level: 0.0,
            processing,
        })
    }
    fn tick(
        &mut self,
        stop: &AtomicBool,
        pressed: &AtomicBool,
        inhibit: &AtomicBool,
        debug: &mut recording::Recorder,
    ) -> Result<(), failure::Failure> {
        self.devices.control.counts.drain(&self.processing.metrics);
        self.devices
            .health
            .check()
            .map_err(|e| failure::Failure::new("device_health", "device_unhealthy", e))?;
        let resets = self.processing.resets;
        self.processing
            .render_tapped(&mut self.devices.reference, &mut |frame, gap| {
                debug.block(
                    recording::Track::Speaker,
                    frame.at,
                    &frame.samples,
                    gap.into(),
                    0,
                    None,
                );
            })?;
        if self.processing.resets != resets {
            self.clear_gate();
        }
        if self.outgoing.is_some() {
            for (remote, writer) in self.remotes.iter_mut().zip(self.devices.writers.iter_mut()) {
                if let Some(remote) = remote {
                    remote.set_deafened(
                        writer,
                        self.devices.control.deafen.load(Ordering::Acquire),
                        Instant::now(),
                    );
                    remote.refill(writer, Instant::now());
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
            self.devices.writers[0].write(&low);
            self.devices.writers[1].write(&high);
            return Ok(());
        }
        // Drain complete 20 ms packets, including large device callbacks. The
        // ring itself bounds latency; trimming each callback to 60 ms destroys
        // capture continuity and prevents the echo filter from converging.
        for _ in 0..CAPACITY / FRAME {
            self.capture_boundary(self.transmit_control.generation(), Instant::now());
            if let Some(at) = self.record_boundary.take() {
                debug.capture_boundary(at);
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            let generation = self.transmit_control.generation();
            let Some(packet) = self.processing.capture_tapped(
                &mut self.devices.capture,
                &mut |frame, gap, reset, delay| {
                    debug.block(
                        recording::Track::Microphone,
                        frame.at,
                        &frame.samples,
                        recording::Continuity { gap, reset },
                        delay,
                        None,
                    );
                },
            )?
            else {
                break;
            };
            if stop.load(Ordering::Acquire) {
                break;
            }
            self.devices
                .control
                .pressed
                .store(pressed.load(Ordering::Acquire), Ordering::Relaxed);
            self.capture_packet(packet, generation, inhibit, debug)?;
        }
        Ok(())
    }
    fn capture_packet(
        &mut self,
        captured: processing::Captured,
        generation: u64,
        inhibit: &AtomicBool,
        debug: &mut recording::Recorder,
    ) -> Result<(), failure::Failure> {
        let media_position = self.media_clock.position(captured.at[0], captured.position);
        let oldest_capture =
            outgoing::oldest_capture(captured.at[0], self.processing.enhancement_delay_samples());
        let peak = captured.pcm.iter().fold(0.0_f32, |p, v| p.max(v.abs()));
        self.processed_level = peak;
        if self
            .gate
            .discontinuous(captured.at[0], captured.gap.iter().any(|gap| *gap))
        {
            self.clear_gate();
        }
        let allowed = !inhibit.load(Ordering::Acquire)
            && generation == self.capture_epoch
            && self
                .transmit_control
                .fresh_capture(generation, oldest_capture)
            && self.connection.as_ref().is_none_or(|c| c.active())
            && self
                .capture_after
                .is_none_or(|after| captured.at[0] >= after)
            && !self.devices.control.mute.load(Ordering::Relaxed)
            && !self.devices.control.deafen.load(Ordering::Relaxed);
        let gate::Output { pcm, active } = self.gate.process(
            captured.pcm,
            captured.evidence,
            captured.at[0],
            captured.gap.iter().any(|g| *g),
            self.settings.mode,
            self.settings.activation_threshold,
            allowed,
            self.devices.control.pressed.load(Ordering::Acquire),
        );
        let mut packet = [0_u8; 4000];
        if self.reset_codec
            || generation != self.encoder_generation
            || (!active && self.encoder_active)
        {
            self.encoder
                .reset()
                .map_err(|e| failure::Failure::opus("encoder_reset", e))?;
            self.decoder
                .reset_state()
                .map_err(|e| failure::Failure::opus("decoder_reset", e))?;
            self.reset_codec = false;
        }
        self.encoder_generation = generation;
        self.encoder_active = active;
        self.encoder
            .adapt(
                self.settings.adaptive_fec,
                5, // Encoder combines the measured policy with its DRED 20% floor.
                self.connection.as_ref().and_then(|c| c.uplink_sample(0)),
                Instant::now(),
            )
            .map_err(|e| failure::Failure::opus("adaptive_fec", e))?;
        let timer = self.processing.metrics.timed(Stage::Encode);
        let (n, omitted, marker) = self
            .encoder
            .encode(&pcm, active, &mut packet)
            .map_err(|e| failure::Failure::opus("encode", e))?;
        drop(timer);
        if !omitted
            && self.outgoing.is_some()
            && let Some(connection) = &self.connection
        {
            connection.metrics.add(Count::OutgoingOfferedPackets, 1);
        }
        // An urgent action may arrive while DSP or Opus runs. Discard that work
        // and clear codec lookbehind as well as pre-roll at the next boundary.
        if generation != self.transmit_control.generation()
            || self.connection.as_ref().is_some_and(|c| !c.active())
        {
            if !omitted
                && self.outgoing.is_some()
                && let Some(connection) = &self.connection
            {
                connection.metrics.add(Count::OutgoingInvalidatedPackets, 1);
            }
            self.devices
                .control
                .transmitting
                .store(false, Ordering::Release);
            return Ok(());
        }
        let gate_delay = if self.settings.mode == TransmitMode::VoiceActivity {
            gate::DELAY
        } else {
            Duration::ZERO
        };
        let oldest_capture = oldest_capture
            .checked_sub(gate_delay)
            .unwrap_or(oldest_capture);
        if let Some(outgoing) = &self.outgoing {
            let connection = self.connection.as_ref().ok_or_else(|| {
                failure::Failure::new("transmit", "missing_connection", "Missing voice connection")
            })?;
            let accepted = if omitted {
                false
            } else {
                let mut frame = outgoing::EncodedFrame {
                    packet_redundancy: self.settings.packet_redundancy,
                    accounted: false,
                    payload: bytes::Bytes::copy_from_slice(&packet[..n]),
                    media_position,
                    marker,
                    oldest_capture,
                    generation,
                    connection: connection.clone(),
                    delivery: self.delivery.clone(),
                    activation_delay: gate_delay,
                };
                match self
                    .transmit_control
                    .check(&frame, connection, Instant::now())
                {
                    Err(reason) => {
                        self.transmit_control.dropped_frame(reason, &mut frame);
                        false
                    }
                    Ok(()) => match outgoing.try_send(frame) {
                        Ok(()) => true,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(mut frame)) => {
                            self.transmit_control
                                .dropped_frame(outgoing::DropReason::QueueFull, &mut frame);
                            false
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(mut frame)) => {
                            self.transmit_control.dropped_frame(
                                outgoing::DropReason::ControlInvalidated,
                                &mut frame,
                            );
                            false
                        }
                    },
                }
            };
            if accepted {
                self.encoder.stats.queued_packets += 1;
                self.encoder.stats.queued_bytes += n as u64;
            } else if !omitted {
                self.encoder.stats.queue_drops += 1;
            }
            self.devices
                .control
                .transmitting
                .store(active && accepted, Ordering::Release);
            for (index, frame) in pcm.as_chunks::<480>().0.iter().enumerate() {
                debug.block(
                    recording::Track::Transmit,
                    captured.at[index],
                    frame,
                    recording::Continuity {
                        gap: captured.gap[index],
                        reset: false,
                    },
                    0,
                    Some((
                        accepted,
                        active,
                        if self.settings.mode == TransmitMode::VoiceActivity {
                            gate::PRE_ROLL * FRAME
                        } else {
                            0
                        },
                    )),
                );
            }
            return Ok(());
        }
        let mut decoded = [0.0; FRAME];
        let timer = self.processing.metrics.timed(Stage::Decode);
        let n = self
            .decoder
            .decode_float(&packet[..n], &mut decoded, false)
            .map_err(|e| failure::Failure::opus("loopback_decode", e))?;
        drop(timer);
        let written = self.devices.writers[0].write(&decoded[..n]);
        self.devices
            .control
            .transmitting
            .store(active && written > 0, Ordering::Release);
        self.processing
            .metrics
            .add(Count::PcmOverflowSamples, (n - written) as u64);
        Ok(())
    }
    fn collect_metrics(&self) {
        self.devices.control.counts.drain(&self.processing.metrics);
        for (remote, writer) in self.remotes.iter().zip(&self.devices.writers) {
            if let Some(remote) = remote {
                writer.control.counts.drain(&remote.jitter.quality);
            }
        }
    }
    fn quality(&self) -> AudioQuality {
        self.collect_metrics();
        let metrics = &self.processing.metrics;
        let network = metrics.network();
        // These legacy cumulative counters are maintained separately; new
        // callback counters are drained exactly once on the worker.
        AudioQuality {
            adaptive_fec: Some(self.encoder.adaptive_diagnostics(Instant::now())),
            metrics: metrics.snapshot(),
            streams: self
                .remotes
                .iter()
                .enumerate()
                .filter_map(|(slot, r)| {
                    r.as_ref().map(|r| {
                        let writer = &self.devices.writers[slot];
                        let pcm_samples = writer.occupied_samples();
                        StreamQuality {
                            slot,
                            metrics: r.jitter.quality.snapshot(),
                            jitter_packets: r.jitter.metrics().packets,
                            pcm_samples,
                            buffered_playout_ms: r.jitter.metrics().packets as f64 * 20.0
                                + pcm_samples as f64 / 48.0,
                            verified_fec_packets: None,
                        }
                    })
                })
                .collect(),
            capture_blocks: self.devices.capture.len(),
            reference_blocks: self.devices.reference.len(),
            sfu_hop_rtt_ms: network.rtt_ms,
            sfu_hop_received_packets: network.received,
            sfu_hop_lost_packets: network.lost,
            ..Default::default()
        }
    }
    fn status(&self) -> AudioStatus {
        AudioStatus {
            ui_performance: None,
            quality: Some(self.quality()),
            recording: None,
            running: true,
            input_level: self.processed_level,
            raw_input_level: f32::from_bits(self.devices.control.peak.load(Ordering::Relaxed)),
            processing_resets: self.processing.resets,
            echo: self.processing.echo_diagnostics(),
            transmitting: self.devices.control.transmitting.load(Ordering::Relaxed),
            message: if let Some(message) = self.recovery.message() {
                message.into()
            } else if self.outgoing.is_some() {
                self.dred_voice_status()
            } else if self.microphone {
                "Microphone test: local Opus loopback (use headphones)".into()
            } else {
                "Output test: two independent streams".into()
            },
            streams: if self.outgoing.is_some() {
                self.remotes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        r.as_ref().map(|r| StreamLevel {
                            speaking: r.activity.speaking(Instant::now()),
                            id: i.to_string(),
                            target: self.volumes.as_ref().map(|p| SpeakerVolumeTarget {
                                guild_id: p.guild_id,
                                account_id: r.id,
                                shared_audio: i >= thiscord_shared::voice::ROOM_CAPACITY,
                            }),
                            label: r.label.clone(),
                            volume: f32::from_bits(
                                self.devices.writers[i]
                                    .control
                                    .volume
                                    .load(Ordering::Relaxed),
                            ),
                        })
                    })
                    .collect()
            } else {
                (0..if self.microphone { 1 } else { 2 })
                    .map(|i| StreamLevel {
                        speaking: false,
                        id: i.to_string(),
                        target: None,
                        label: if self.microphone {
                            "Microphone loopback".into()
                        } else {
                            format!("Test stream {}", i + 1)
                        },
                        volume: f32::from_bits(
                            self.devices.writers[i]
                                .control
                                .volume
                                .load(Ordering::Relaxed),
                        ),
                    })
                    .collect()
            },
            dropped_samples: self.devices.control.dropped.load(Ordering::Relaxed)
                + self
                    .devices
                    .control
                    .reference_dropped
                    .load(Ordering::Relaxed),
            underrun_samples: self.devices.control.underruns.load(Ordering::Relaxed),
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
        ui_performance: None,
        quality: None,
        recording: None,
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
fn lease_expired(voice: bool, elapsed: Duration) -> bool {
    elapsed > Duration::from_secs(if voice { 15 } else { 3 })
}
#[allow(clippy::too_many_arguments)]
fn run(
    receiver: &mpsc::Receiver<Request>,
    packets: &mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    pressed: Arc<AtomicBool>,
    inhibit: Arc<AtomicBool>,
    debug_stop: Arc<AtomicBool>,
    deafen: Arc<AtomicBool>,
    probe: &worker_health::Probe,
) {
    let mut debug = recording::Recorder::default();
    let mut session: Option<Session> = None;
    let mut pending: Option<reconfigure::Pending> = None;
    let mut message: String = if probe.restarts.load(Ordering::Relaxed) > 0 {
        "Audio worker restarted after an internal failure. Join voice again; open diagnostic logs."
            .into()
    } else {
        "Audio stopped".into()
    };
    let mut lease = Instant::now();
    let mut previous = Instant::now();
    let mut last_snapshot = Instant::now();
    loop {
        probe.enter(worker_health::IDLE);
        probe.active.store(session.is_some(), Ordering::Release);
        if let Some(s) = &mut session {
            s.capture_boundary(s.transmit_control.generation(), Instant::now());
        }
        if session
            .as_ref()
            .is_some_and(|s| s.connection.as_ref().is_some_and(|c| !c.active()))
        {
            stop.store(true, Ordering::Release);
        }
        debug.poll();
        if debug_stop.swap(false, Ordering::AcqRel) {
            debug.stop();
        }
        // Release requests and PTT key-up cannot be lost behind a full media queue.
        if stop.swap(false, Ordering::AcqRel) {
            if let Some(pending) = &mut pending {
                pending.cancel("Audio stopped; pending settings were cancelled");
            }
            debug.stop();
            session = None;
            pressed.store(false, Ordering::Release);
            message = "Audio stopped; devices released".into();
        }
        if previous.elapsed() > Duration::from_secs(2) {
            diagnostics::event(
                "worker_scheduling_gap",
                serde_json::json!({"elapsed_ms":previous.elapsed().as_millis(),"voice":session.as_ref().is_some_and(|s| s.connection.is_some())}),
            );
        }
        if session
            .as_ref()
            .is_some_and(|s| lease_expired(s.connection.is_some(), lease.elapsed()))
        {
            diagnostics::event(
                "audio_lease_expired",
                serde_json::json!({"elapsed_ms":lease.elapsed().as_millis(),"voice":session.as_ref().is_some_and(|s|s.connection.is_some())}),
            );
            if let Some(pending) = &mut pending {
                pending.cancel("Audio suspended; pending settings were cancelled");
            }
            debug.stop();
            session = None;
            message = "Audio stopped after inactivity or suspend".into();
        }
        previous = Instant::now();
        if last_snapshot.elapsed() >= Duration::from_secs(10) {
            last_snapshot = Instant::now();
            if let Some(s) = &session {
                diagnostics::quality(|| s.quality());
                diagnostics::event(
                    "audio_snapshot",
                    serde_json::json!({"processing_resets":s.processing.resets,"processing_reset_causes":s.processing.reset_causes,"opus_decode":s.remotes.iter().flatten().map(|r| &r.decode_stats).collect::<Vec<_>>(),"verified_fec_frames":null,"playout":s.remotes.iter().flatten().map(|r|r.jitter.metrics()).collect::<Vec<_>>(),"receive":s.remotes.iter().flatten().map(|r|r.jitter.diagnostics()).collect::<Vec<_>>(),"health":s.devices.health.diagnostics(),"recovering":s.recovery.active(),"muted":s.settings.muted,"deafened":s.settings.deafened,"deep_redundancy":s.settings.deep_redundancy,"dred_bitrate_target":if s.settings.deep_redundancy { dred::BITRATE } else { 32_000 },"echo":s.settings.echo_cancellation,"neural_echo":s.settings.neural_echo,"suppression":s.settings.noise_suppression,"suppression_model":s.settings.noise_suppression_model,"automatic_gain":s.settings.automatic_gain,"capture_overflow_samples":s.devices.control.dropped.load(Ordering::Relaxed),"playback_starvation_samples":s.devices.control.underruns.load(Ordering::Relaxed)}),
                );
            }
        }
        let request = match receiver.try_recv() {
            Ok(request) => Ok(request),
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => match packets.try_recv() {
                Ok(packet) => Ok((packet, None)),
                Err(_) => receiver.recv_timeout(Duration::from_millis(5)),
            },
        };
        match request {
            Ok((command, mut reply)) => {
                probe.enter(command.operation());
                if matches!(
                    command,
                    Command::Start { .. }
                        | Command::VoiceStart { .. }
                        | Command::Settings(_)
                        | Command::Stop
                ) {
                    diagnostics::event(
                        "audio_command",
                        serde_json::json!({"operation":command.operation()}),
                    );
                }
                if !matches!(
                    &command,
                    Command::Peek
                        | Command::CurrentSettings(_)
                        | Command::Packet { .. }
                        | Command::Roster { .. }
                        | Command::ReceiveBoundary { .. }
                        | Command::Keepalive(_)
                ) && session.as_ref().is_none_or(|s| s.connection.is_none())
                {
                    lease = Instant::now();
                }
                if matches!(
                    &command,
                    Command::Start { .. } | Command::VoiceStart { .. } | Command::Stop
                ) && let Some(pending) = &mut pending
                {
                    pending.cancel("Audio session changed; pending settings were cancelled");
                }
                let result = (|| -> Result<(), String> {
                    match command {
                        Command::DebugStart(parent) => {
                            let s = session
                                .as_ref()
                                .filter(|s| s.microphone && s.outgoing.is_some())
                                .ok_or(
                                    "Join a voice channel with microphone access before recording",
                                )?;
                            if s.recovery.active() {
                                return Err(
                                    "Wait for audio device recovery before recording".into()
                                );
                            }
                            debug.start(parent, s.settings.clone(), s.devices.info.clone())?;
                        }
                        Command::DebugStop => debug.stop(),
                        Command::Start {
                            settings,
                            microphone,
                        } => {
                            debug.stop();
                            session = None;
                            session = Some(Session::start(&settings, microphone)?);
                            session.as_mut().unwrap().transmit_control = probe.outgoing.clone();
                            lease = Instant::now();
                            message.clear();
                        }
                        Command::VoiceStart {
                            volumes,
                            connection,
                            settings,
                            outgoing,
                            microphone,
                        } => {
                            if !connection.active() {
                                return Err("Voice connection was cancelled".into());
                            }
                            debug.stop();
                            session = None;
                            let mut s = Session::start(&settings, microphone)?;
                            for writer in &s.devices.writers {
                                writer.control.active.store(false, Ordering::Release);
                            }
                            s.volumes = Some(volumes);
                            s.outgoing = Some(outgoing);
                            s.processing.metrics = connection.metrics.clone();
                            s.transmit_control = probe.outgoing.clone();
                            let _ = s
                                .devices
                                .control
                                .receive_guard
                                .set(s.transmit_control.clone());
                            s.connection = Some(connection);
                            session = Some(s);
                            lease = Instant::now();
                        }
                        command @ (Command::Packet { .. }
                        | Command::Roster { .. }
                        | Command::ReceiveBoundary { .. }) => {
                            if let Some(s) = &mut session {
                                s.receive_command(
                                    command,
                                    deafen.load(Ordering::Acquire),
                                    Instant::now(),
                                )?;
                            }
                        }
                        Command::Stop => {
                            debug.stop();
                            session = None;
                            message = "Audio stopped; devices released".into();
                        }
                        Command::Settings(settings) => {
                            settings.validate()?;
                            if pending.is_some() {
                                return Err(
                                    "An audio change is still being prepared; try again shortly"
                                        .into(),
                                );
                            }
                            if let Some(s) = &mut session {
                                let devices = settings.input != s.settings.input
                                    || settings.output != s.settings.output;
                                if devices || s.processing.needs_replacement(&settings) {
                                    if s.recovery.active() {
                                        return Err(
                                            "Audio devices are recovering; try again shortly"
                                                .into(),
                                        );
                                    }
                                    pending = Some(reconfigure::Pending::start(
                                        settings,
                                        devices,
                                        s.microphone,
                                        reply.clone(),
                                    )?);
                                    reply = None; // The preparation commits/replies asynchronously.
                                    return Ok(());
                                }
                                s.processing.settings(&settings)?;
                                s.set_deep_redundancy(settings.deep_redundancy)?;
                                s.settings = settings.clone();
                                debug.settings(&settings);
                                for (remote, writer) in s.remotes.iter_mut().zip(&s.devices.writers)
                                {
                                    if let Some(remote) = remote {
                                        remote.set_deafened(
                                            writer,
                                            settings.deafened,
                                            Instant::now(),
                                        );
                                    }
                                }
                                apply(&s.devices.control, &settings);
                            }
                        }
                        Command::CurrentSettings(reply) => {
                            let _ = reply.send(session.as_ref().map(|s| s.settings.clone()));
                        }
                        Command::Volume {
                            stream,
                            gain,
                            target,
                        } => {
                            let s = session.as_mut().ok_or("Audio is stopped")?;
                            s.set_volume(stream, target, gain)?;
                        }
                        Command::Pressed(_) => {}
                        Command::Keepalive(connection) => {
                            if session
                                .as_ref()
                                .and_then(|s| s.connection.as_ref())
                                .is_some_and(|c| c.accepts(&connection))
                            {
                                lease = Instant::now();
                            }
                        }
                        Command::Notification { kind, volume } => {
                            if let Some(s) = &session {
                                let index = crate::notification_sound::mixer_index(kind);
                                s.devices.control.cue_volumes[index]
                                    .store(volume.min(100), Ordering::Relaxed);
                                if !s.devices.control.deafen.load(Ordering::Relaxed) {
                                    s.devices
                                        .control
                                        .cues
                                        .fetch_or(1 << index, Ordering::Relaxed);
                                }
                            }
                        }
                        Command::Status | Command::Peek => {}
                        #[cfg(test)]
                        Command::CrashWorker => panic!("injected audio worker failure"),
                    }
                    Ok(())
                })();
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| {
                        let mut status = session
                            .as_ref()
                            .map(|s| s.status())
                            .unwrap_or_else(|| stopped(message.clone()));
                        status.recording = debug.status();
                        status
                    }));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Commit only between complete processing ticks. Stop takes priority
        // even if it arrived while a replacement was finishing.
        if stop.load(Ordering::Acquire)
            || session
                .as_ref()
                .is_some_and(|s| s.connection.as_ref().is_some_and(|c| !c.active()))
        {
            continue;
        }
        if let Some(s) = &mut session
            && !s.recovery.active()
            && let Err(error) = s.devices.health.check().and_then(|_| {
                s.device_watch
                    .check(&s.devices.health, s.microphone, Instant::now())
            })
        {
            if let Some(p) = &mut pending {
                p.cancel("Audio device failed; settings change cancelled");
            }
            debug.stop();
            diagnostics::event(
                "device_failure",
                serde_json::json!({"error":error,"device":s.devices.info,"health":s.devices.health.diagnostics()}),
            );
            s.transmit_control.invalidate();
            s.clear_gate();
            s.processed_level = 0.0;
            s.devices.control.peak.store(0, Ordering::Relaxed);
            // Release the failed endpoints before attempting to reopen them.
            s.devices.streams = Box::new(SilentStreams);
            s.recovery.begin(format!(
                "{error} Microphone: {}; output: {}.",
                s.devices.info["input"]["name"]
                    .as_str()
                    .unwrap_or("not active"),
                s.devices.info["output"]["name"]
                    .as_str()
                    .unwrap_or("unknown")
            ));
        }
        if stop.load(Ordering::Acquire)
            && let Some(pending) = &mut pending
        {
            pending.cancel("Audio stopped; pending settings were cancelled");
        }
        if pending
            .as_mut()
            .is_some_and(|p| p.poll(&mut session, &mut debug, deafen.load(Ordering::Acquire)))
        {
            pending = None;
        }
        if let Some(s) = &mut session {
            if s.recovery.active() {
                probe.enter(worker_health::RECOVERY);
                // A cancelled settings worker must finish before another open.
                if pending.is_some() {
                    if s.recovery.wait_expired() {
                        session = None;
                        message = "Audio recovery timed out waiting for a previous device operation. Process information is unavailable. Join again after checking the device.".into();
                    }
                    continue;
                }
                match s.recovery.poll(&s.settings, s.microphone) {
                    Ok(Some(devices)) => {
                        match s.resume_devices(devices, &stop, &pressed, &deafen) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(error) => {
                                s.recovery
                                    .begin(format!("Cannot restart audio devices: {error}"));
                                continue;
                            }
                        }
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        session = None;
                        message = error;
                        continue;
                    }
                }
            }
            if deafen.load(Ordering::Acquire) {
                s.devices.control.deafen.store(true, Ordering::Release);
            }
            s.devices
                .control
                .pressed
                .store(pressed.load(Ordering::Acquire), Ordering::Relaxed);
            if s.outgoing.is_none()
                && s.started.elapsed() > Duration::from_secs(if s.microphone { 60 } else { 5 })
            {
                session = None;
                message = "Test finished; devices released".into();
            } else if let Err(error) = {
                probe.enter(worker_health::PROCESS);
                let started = Instant::now();
                let result = s.tick(&stop, &pressed, &inhibit, &mut debug);
                probe.processed(started.elapsed());
                result
            } {
                // A callback may fail between the health check above and tick.
                // Let the next iteration enter recovery instead of disconnecting.
                if s.devices.health.check().is_err() {
                    continue;
                }
                if let Some(pending) = &mut pending {
                    pending.cancel("Audio device failed; pending settings were cancelled");
                }
                debug.stop();
                let mut details = serde_json::json!({"reason":"processing_tick_failed", "error":error.details(), "health":s.devices.health.diagnostics(), "echo":s.settings.echo_cancellation, "suppression":s.settings.noise_suppression, "neural_echo":s.settings.neural_echo});
                if let Some(connection) = &s.connection {
                    details = connection.diagnostic_details(details);
                }
                diagnostics::event("audio_processing_failed", details);
                session = None;
                message = error.into();
            }
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn worker_panic_is_recoverable_and_restart_budget_is_finite() {
        let engine = AudioEngine::new();
        for _ in 0..3 {
            assert!(engine.command(Command::CrashWorker).is_err());
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(status) = engine.command(Command::Peek) {
                    assert!(!status.running);
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            assert!(engine.inhibit.load(Ordering::Acquire));
            assert!(!engine.pressed.load(Ordering::Acquire));
        }
        assert!(engine.command(Command::CrashWorker).is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if engine
                .command(Command::Peek)
                .is_err_and(|e| e.contains("has exited"))
            {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn game_launch_delay_is_not_a_voice_suspend() {
        assert!(!lease_expired(true, Duration::from_secs(5)));
        assert!(lease_expired(true, Duration::from_secs(16)));
        assert!(lease_expired(false, Duration::from_secs(4)));
    }
    #[test]
    fn cancelled_voice_start_never_opens_devices_or_relaxes_mute() {
        let engine = AudioEngine::new();
        engine.restrict(&AudioSettings {
            muted: true,
            ..Default::default()
        });
        let connection = connection::Connection::default();
        connection.close();
        let result = engine.command(Command::VoiceStart {
            volumes: volumes::Store::default()
                .profile("00000000-0000-0000-0000-000000000001".parse().unwrap()),
            connection,
            settings: AudioSettings::default(),
            outgoing: tokio::sync::mpsc::channel(1).0,
            microphone: true,
        });
        assert!(result.is_err());
        assert!(engine.inhibit.load(Ordering::Acquire));
        assert!(!engine.command(Command::Peek).unwrap().running);
        assert!(engine.current_settings().unwrap().is_none());
    }
    #[test]
    fn integer_microphone_samples_convert_to_normalized_mono() {
        assert_eq!(capture_mono(&[cpal::I24::new(4194304).unwrap()]), 0.5);
        assert_eq!(capture_mono(&[1073741824_i32, -536870912_i32]), 0.125);
        assert_eq!(capture_mono(&[32768_u16, 32768]), 0.0);
        assert_eq!(capture_mono(&[f32::NAN]), 0.0);
    }
    #[test]
    fn saturated_media_queue_cannot_lose_stop_or_ptt_release() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (media, _packets) = mpsc::sync_channel(1);
        let engine = AudioEngine {
            sender,
            media,
            probe: worker_health::Probe::new(),
            stop: Arc::new(AtomicBool::new(false)),
            pressed: Arc::new(AtomicBool::new(true)),
            inhibit: Arc::new(AtomicBool::new(false)),
            debug_stop: Arc::new(AtomicBool::new(false)),
            deafen: Arc::new(AtomicBool::new(false)),
        };
        let connection = connection::Connection::default();
        let queued = outgoing::EncodedFrame {
            packet_redundancy: false,
            accounted: false,
            marker: false,
            payload: bytes::Bytes::from_static(b"synthetic"),
            media_position: 0,
            oldest_capture: Instant::now(),
            generation: engine.probe.outgoing.generation(),
            connection: connection.clone(),
            delivery: Default::default(),
            activation_delay: Duration::ZERO,
        };
        for _ in 0..10 {
            engine.notify(Command::Packet {
                payload_type: 111,
                source_epoch: None,
                privacy_epoch: None,
                marker: false,
                connection: Default::default(),
                slot: 0,
                sequence: 0,
                timestamp: 0,
                received_at: Instant::now(),
                payload: bytes::Bytes::new(),
            });
        }
        assert_eq!(engine.probe.packet_drops.load(Ordering::Relaxed), 9);
        engine.notify(Command::Peek);
        assert!(matches!(receiver.try_recv().unwrap().0, Command::Peek));
        engine.notify(Command::Peek);
        assert!(
            engine
                .command(Command::Peek)
                .unwrap_err()
                .contains("queue is full")
        );
        engine.notify(Command::Pressed(false));
        engine.notify(Command::Pressed(true));
        assert_eq!(
            engine
                .probe
                .outgoing
                .check(&queued, &connection, Instant::now()),
            Err(outgoing::DropReason::ControlInvalidated)
        );
        engine.notify(Command::Pressed(false));
        engine.notify(Command::Stop);
        engine.notify(Command::DebugStop);
        engine.notify(Command::Settings(AudioSettings {
            muted: true,
            ..Default::default()
        }));
        assert!(!engine.pressed.load(Ordering::Acquire));
        assert!(engine.stop.load(Ordering::Acquire));
        assert!(engine.debug_stop.load(Ordering::Acquire));
        assert!(engine.inhibit.load(Ordering::Acquire));
        assert_eq!(
            engine
                .probe
                .outgoing
                .check(&queued, &connection, Instant::now()),
            Err(outgoing::DropReason::ControlInvalidated)
        );
        drop(receiver);
        assert!(
            engine
                .command(Command::Peek)
                .unwrap_err()
                .contains("has exited")
        );
        engine.restrict(&AudioSettings {
            deafened: true,
            ..Default::default()
        });
        engine.restrict(&AudioSettings::default()); // Restrictions cannot unmute/deafen.
        assert!(engine.deafen.load(Ordering::Acquire));
        assert!(engine.inhibit.load(Ordering::Acquire));
    }

    #[test]
    fn urgent_mute_deafen_stop_and_replacement_invalidate_before_queue_drain() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let (media, _packets) = mpsc::sync_channel(1);
        let engine = AudioEngine {
            sender,
            media,
            probe: worker_health::Probe::new(),
            stop: Default::default(),
            pressed: Default::default(),
            inhibit: Default::default(),
            debug_stop: Default::default(),
            deafen: Default::default(),
        };
        for command in [
            Command::Settings(AudioSettings {
                muted: true,
                ..Default::default()
            }),
            Command::Settings(AudioSettings {
                deafened: true,
                ..Default::default()
            }),
            Command::Pressed(false),
            Command::Stop,
        ] {
            let connection = connection::Connection::default();
            let frame = outgoing::EncodedFrame {
                packet_redundancy: false,
                accounted: false,
                marker: false,
                payload: bytes::Bytes::from_static(b"synthetic"),
                media_position: 0,
                oldest_capture: Instant::now(),
                generation: engine.probe.outgoing.generation(),
                connection: connection.clone(),
                delivery: connection::Connection::default(),
                activation_delay: Duration::ZERO,
            };
            engine.notify(command);
            // Relax before draining; old encoded speech remains invalid.
            engine.notify(Command::Settings(AudioSettings::default()));
            engine.notify(Command::Pressed(true));
            assert_eq!(
                engine
                    .probe
                    .outgoing
                    .check(&frame, &connection, Instant::now()),
                Err(outgoing::DropReason::ControlInvalidated)
            );
        }
        engine.probe.exited();
    }
}
