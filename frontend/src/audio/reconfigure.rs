//! One bounded preparation job. The audio worker keeps servicing media while
//! models/devices are opened, and commits only to the session that requested it.
use super::*;

pub(super) struct Replacement {
    processing: processing::Prepared,
    devices: Option<Devices>,
}

pub(super) struct Pending {
    settings: AudioSettings,
    result: mpsc::Receiver<Result<Replacement, String>>,
    retire: mpsc::Sender<Replacement>,
    reply: Option<mpsc::Sender<Result<AudioStatus, String>>>,
    started: Instant,
    cancelled: bool,
}
impl Pending {
    pub fn start(
        settings: AudioSettings,
        devices: bool,
        microphone: bool,
        reply: Option<mpsc::Sender<Result<AudioStatus, String>>>,
    ) -> Result<Self, String> {
        let requested = settings.clone();
        let (tx, result) = mpsc::channel();
        let (retire, retired) = mpsc::channel();
        thread::Builder::new()
            .name("thiscord-audio-prepare".into())
            .spawn(move || {
                let prepared = (|| {
                    let processing = processing::Prepared::new(&requested)?;
                    let devices = devices
                        .then(|| Devices::prepare(&requested, microphone))
                        .transpose()?;
                    Ok(Replacement {
                        processing,
                        devices,
                    })
                })();
                if tx.send(prepared).is_ok() {
                    // Destruction of the old streams/model also stays off the
                    // processing worker. Cancellation simply closes this channel.
                    drop(retired.recv());
                }
            })
            .map_err(|_| "Cannot start audio preparation")?;
        Ok(Self {
            settings,
            result,
            retire,
            reply,
            started: Instant::now(),
            cancelled: false,
        })
    }
    pub fn cancel(&mut self, reason: &str) {
        if !self.cancelled {
            diagnostics::event(
                "settings_preparation_cancelled",
                serde_json::json!({"elapsed_ms":self.started.elapsed().as_millis()}),
            );
        }
        self.cancelled = true;
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err(reason.into()));
        }
    }
    /// Returns true once the job has been collected. Cancelled/timed-out jobs
    /// continue occupying the single slot until their worker actually finishes.
    pub fn poll(
        &mut self,
        session: &mut Option<Session>,
        debug: &mut recording::Recorder,
        deafened: bool,
    ) -> bool {
        if self.started.elapsed() >= Duration::from_secs(8) {
            self.cancel("Audio change timed out; the previous setup is still active");
        }
        let result = match self.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("Audio preparation stopped".into()),
        };
        if session.is_none() {
            self.cancel("Audio session ended; pending settings were cancelled");
        }
        if self.cancelled {
            if let Ok(prepared) = result {
                let _ = self.retire.send(prepared);
            }
            return true;
        }
        let result = result.and_then(|mut prepared| {
            let s = session.as_mut().expect("session checked above");
            if let Some(next) = &mut prepared.devices {
                next.control
                    .deafen
                    .store(self.settings.deafened || deafened, Ordering::Release);
                // Opened but paused during preparation: never play stale capture
                // or briefly double the remote audio while loading a model.
                if let Err(error) = next.play() {
                    let _ = self.retire.send(prepared);
                    return Err(error);
                }
                debug.stop(); // New device clocks/formats require a new recording.
                next.inherit(&s.devices);
                s.collect_metrics();
                std::mem::swap(&mut s.devices, next);
                s.media_clock.restart_device();
                s.transmit_control.invalidate();
                for (remote, writer) in s.remotes.iter_mut().zip(&s.devices.writers) {
                    if let Some(remote) = remote {
                        remote.clear(writer, Instant::now());
                    }
                }
                s.device_watch = Default::default();
                diagnostics::event("device_reconfigured", s.devices.info.clone());
                s.recovery.reconfigured(&s.devices.info);
            } else {
                debug.settings(&self.settings);
            }
            prepared.processing = s.processing.install(prepared.processing)?;
            s.reset_gate(Instant::now());
            s.processed_level = 0.0;
            s.set_deep_redundancy(self.settings.deep_redundancy)?;
            s.settings = self.settings.clone();
            apply(&s.devices.control, &self.settings);
            if deafened {
                s.devices.control.deafen.store(true, Ordering::Release);
            }
            let _ = self.retire.send(prepared);
            let mut status = s.status();
            status.recording = debug.status();
            Ok(status)
        });
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeStreams(bool);
    impl DeviceStreams for FakeStreams {
        fn play(&self) -> Result<(), String> {
            if self.0 {
                Ok(())
            } else {
                Err("device refused start".into())
            }
        }
    }
    fn devices(works: bool) -> Devices {
        let control = Arc::new(Controls::default());
        let (writers, _) = mixer(control.clone());
        Devices {
            info: serde_json::json!({"fake":true}),
            streams: Box::new(FakeStreams(works)),
            capture: frames::queue().1,
            reference: frames::queue().1,
            writers,
            control,
            health: Arc::new(StreamHealth::default()),
        }
    }
    fn session() -> Session {
        Session {
            device_watch: Default::default(),
            recovery: Default::default(),
            volumes: None,
            connection: None,
            settings: AudioSettings::default(),
            devices: devices(true),
            encoder: transmit::Encoder::new(true).unwrap(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
            microphone: true,
            phase: 0.0,
            next: Instant::now(),
            started: Instant::now(),
            gate: Default::default(),
            capture_epoch: 0,
            capture_after: None,
            record_boundary: None,
            reset_codec: false,
            delivery: Default::default(),
            outgoing: Some(tokio::sync::mpsc::channel(2).0),
            media_clock: Default::default(),
            transmit_control: Default::default(),
            encoder_generation: 0,
            encoder_active: false,
            remotes: (0..MAX_STREAMS).map(|_| None).collect(),
            receive_fences: Default::default(),
            receive_floor: 0,
            processed_level: 0.5,
            processing: processing::Processing::new(&AudioSettings::default()).unwrap(),
        }
    }
    #[test]
    fn device_recovery_preserves_numeric_totals_but_snapshot_omits_identity() {
        let mut s = session();
        s.devices.control.counts.capture_overflow.add(480);
        s.devices.control.counts.input_samples.add(960);
        s.devices.control.counts.input_clipped.add(3);
        let stop = AtomicBool::new(false);
        let pressed = AtomicBool::new(false);
        let deafen = AtomicBool::new(false);
        assert!(
            s.resume_devices(devices(true), &stop, &pressed, &deafen)
                .unwrap()
        );
        let q = s.quality();
        assert_eq!(q.metrics.total.capture_overflow_samples, 480);
        assert_eq!(q.metrics.total.input_samples, 960);
        assert_eq!(q.metrics.total.input_clipped_samples, 3);
        let text = serde_json::to_string(&q).unwrap();
        for forbidden in [
            "account_id",
            "guild_id",
            "label",
            "payload",
            "token",
            "sdp",
            "model_path",
        ] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn dtx_omissions_are_neither_offered_packets_nor_transport_drops() {
        let mut s = session();
        s.settings.mode = TransmitMode::PushToTalk;
        let connection = connection::Connection::default();
        s.connection = Some(connection.clone());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let at = Instant::now() + Duration::from_secs(1);
        let mut debug = recording::Recorder::default();
        for n in 0..100u64 {
            let time = at + Duration::from_millis(n * 20);
            s.capture_packet(
                processing::Captured {
                    pcm: [0.0; FRAME],
                    evidence: gate::Evidence([0.0; 2]),
                    at: [time, time + Duration::from_millis(10)],
                    gap: [false; 2],
                    position: n * FRAME as u64,
                },
                0,
                &AtomicBool::new(false),
                &mut debug,
            )
            .unwrap();
            if let Ok(mut frame) = rx.try_recv() {
                frame.finish(Count::OutgoingSentPackets);
            }
        }
        assert!(s.encoder.stats.omitted_packets > 50);
        let q = connection.metrics.snapshot().total;
        assert_eq!(
            q.outgoing_offered_packets,
            100 - s.encoder.stats.omitted_packets
        );
        assert_eq!(q.outgoing_sent_packets, q.outgoing_offered_packets);
        assert_eq!(q.outgoing_full_packets, 0);
        assert_eq!(q.outgoing_expired_packets, 0);
        assert_eq!(q.outgoing_invalidated_packets, 0);
    }

    #[test]
    fn preroll_keeps_current_rtp_slots_and_invalidates_pending_audio_on_gaps() {
        let mut s = session();
        s.connection = Some(Default::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let start = Instant::now() + Duration::from_secs(1);
        let mut clock = outgoing::RtpClock::default();
        let mut last = None;
        for n in 0..8 {
            let slot = if n == 7 { 10 } else { n };
            let at = start + Duration::from_millis(slot * 20);
            let level = if n < 2 { 0.003 } else { 0.03 };
            s.capture_packet(
                processing::Captured {
                    pcm: [level; FRAME],
                    evidence: gate::Evidence([level; 2]),
                    at: [at, at + Duration::from_millis(10)],
                    gap: [n == 7, false],
                    position: slot * 960,
                },
                0,
                &AtomicBool::new(false),
                &mut recording::Recorder::default(),
            )
            .unwrap();
            let frame = rx.try_recv().unwrap();
            assert!(
                rx.try_recv().is_err(),
                "One emission per input, including opening"
            );
            assert_eq!(
                clock.next(frame.media_position),
                (n as u16 + 1, slot as u32 * 960)
            );
            assert_eq!(frame.oldest_capture, at - gate::DELAY);
            assert_eq!(frame.activation_delay, gate::DELAY);
            assert_eq!(s.status().transmitting, (3..7).contains(&n));
            if n == 7 {
                assert_eq!(
                    s.transmit_control.check(
                        last.as_ref().unwrap(),
                        s.connection.as_ref().unwrap(),
                        at
                    ),
                    Err(outgoing::DropReason::ControlInvalidated)
                );
            }
            last = Some(frame);
        }
    }

    #[test]
    fn control_change_between_boundary_observation_and_capture_cannot_release_old_preroll() {
        let mut s = session();
        s.connection = Some(Default::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let start = Instant::now() + Duration::from_secs(1);
        let packet = |n: u64| processing::Captured {
            pcm: [0.1; FRAME],
            evidence: gate::Evidence([0.1; 2]),
            at: [
                start + Duration::from_millis(n * 20),
                start + Duration::from_millis(n * 20 + 10),
            ],
            gap: [false; 2],
            position: n * 960,
        };
        for n in 0..4 {
            s.capture_packet(
                packet(n),
                0,
                &AtomicBool::new(false),
                &mut recording::Recorder::default(),
            )
            .unwrap();
            rx.try_recv().unwrap();
        }
        assert!(s.status().transmitting);
        // Simulate a release/repress arriving just after the worker checked its
        // boundary. Fresh capture alone must not authorize the old gate ring.
        s.transmit_control.invalidate();
        let generation = s.transmit_control.generation();
        s.capture_packet(
            packet(4),
            generation,
            &AtomicBool::new(false),
            &mut recording::Recorder::default(),
        )
        .unwrap();
        assert!(!s.status().transmitting);
        assert!(!s.encoder_active);
        let mut listener = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut pcm = [0.0; FRAME];
        listener
            .decode_float(&rx.try_recv().unwrap().payload, &mut pcm, false)
            .unwrap();
        assert!(pcm.iter().all(|v| v.abs() < 4.0 / 32768.0));
    }

    #[test]
    fn session_boundaries_clear_preroll_codec_and_stale_capture_without_resetting_aec() {
        for mode in [TransmitMode::VoiceActivity, TransmitMode::PushToTalk] {
            let mut s = session();
            s.settings.mode = mode;
            let (tx, mut rx) = tokio::sync::mpsc::channel(32);
            s.outgoing = Some(tx);
            let start = Instant::now();
            let no = AtomicBool::new(false);
            s.connection = Some(Default::default());
            s.devices.control.pressed.store(true, Ordering::Release);
            let mut generation = s.transmit_control.generation();
            let mut debug = recording::Recorder::default();
            for index in 0..6 {
                s.capture_packet(
                    processing::Captured {
                        pcm: [0.8; FRAME],
                        evidence: gate::Evidence([0.8; 2]),
                        at: [
                            start + Duration::from_millis(index * 20),
                            start + Duration::from_millis(index * 20 + 10),
                        ],
                        gap: [false; 2],
                        position: index * 960,
                    },
                    generation,
                    &no,
                    &mut debug,
                )
                .unwrap();
                rx.try_recv().unwrap();
            }
            assert!(s.status().transmitting);
            let boundary = start + Duration::from_millis(120);
            let pending_delivery = s.delivery.clone();
            s.transmit_control.invalidate();
            generation = s.transmit_control.generation();
            s.capture_boundary(generation, boundary);
            assert!(!pending_delivery.active());
            assert!(!s.status().transmitting);
            assert_eq!(s.processing.resets, 0);
            let mut listener = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
            // Includes an old device packet delivered *after* relaxation, then
            // fresh silence. No retained audio may emerge from the encoder.
            for index in 0..8 {
                let at = boundary + Duration::from_millis(index * 20) - Duration::from_millis(20);
                s.capture_packet(
                    processing::Captured {
                        pcm: [if index == 0 { 0.8 } else { 0.0 }; FRAME],
                        evidence: gate::Evidence([if index == 0 { 0.8 } else { 0.0 }; 2]),
                        at: [at, at + Duration::from_millis(10)],
                        gap: [false; 2],
                        position: index * 960,
                    },
                    generation,
                    &no,
                    &mut debug,
                )
                .unwrap();
                let mut decoded = [0.0; FRAME];
                listener
                    .decode_float(&rx.try_recv().unwrap().payload, &mut decoded, false)
                    .unwrap();
                let peak = decoded.iter().fold(0.0_f32, |a, v| a.max(v.abs()));
                // Opus may synthesize a few PCM16 LSBs of comfort noise.
                assert!(
                    peak < 4.0 / 32768.0,
                    "mode={mode:?}, packet={index}, decoded peak={peak}"
                );
            }
        }
    }

    #[test]
    fn transmission_indicators_follow_queue_acceptance_and_connection_restrictions() {
        let mut s = session();
        s.settings.mode = TransmitMode::PushToTalk;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let start = Instant::now();
        let no = AtomicBool::new(false);
        s.connection = Some(Default::default());
        s.devices.control.pressed.store(true, Ordering::Release);
        let generation = s.transmit_control.generation();
        let mut debug = recording::Recorder::default();
        let packet = |n: u64| processing::Captured {
            pcm: [0.1; FRAME],
            evidence: gate::Evidence([0.1; 2]),
            at: [
                start + Duration::from_millis(n * 20),
                start + Duration::from_millis(n * 20 + 10),
            ],
            gap: [false; 2],
            position: n * 960,
        };
        s.capture_packet(packet(0), generation, &no, &mut debug)
            .unwrap();
        assert!(s.status().transmitting);
        s.capture_packet(packet(1), generation, &no, &mut debug)
            .unwrap();
        assert!(
            !s.status().transmitting,
            "Full sender queue is not transmitting"
        );
        rx.try_recv().unwrap();
        s.connection = Some(Default::default());
        s.connection.as_ref().unwrap().close();
        s.capture_packet(packet(2), generation, &no, &mut debug)
            .unwrap();
        assert!(!s.status().transmitting);
        assert!(rx.try_recv().is_err());
        s.connection = Some(Default::default());
        s.devices.control.pressed.store(false, Ordering::Release);
        s.capture_packet(packet(3), generation, &no, &mut debug)
            .unwrap();
        assert!(!s.status().transmitting, "PTT release has no tail");
    }
    #[test]
    fn capture_to_opus_queue_preserves_time_under_pressure_and_clears_encoder_speech() {
        let mut s = session();
        s.settings.mode = TransmitMode::PushToTalk;
        s.connection = Some(connection::Connection::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let (mut writer, reader) = frames::queue();
        s.devices.capture = reader;
        let at = Instant::now() + Duration::from_secs(1);
        let no = AtomicBool::new(false);
        let yes = AtomicBool::new(true);
        let mut debug = recording::Recorder::default();
        writer.begin_capture(at);
        for _ in 0..2880 {
            writer.push(0.1);
        }
        s.tick(&no, &yes, &no, &mut debug).unwrap();
        let first = rx.try_recv().unwrap();
        assert_eq!(first.media_position, 0);
        assert_eq!(s.transmit_control.snapshot()["queue_full"], 2);
        assert!(s.encoder_active);
        // This fresh muted packet must be a clean silent encoding, not the
        // tail of an encoder which has just processed nonzero speech.
        s.devices.control.mute.store(true, Ordering::Release);
        writer.begin_capture(at + Duration::from_millis(60));
        for _ in 0..960 {
            writer.push(0.1);
        }
        s.tick(&no, &yes, &no, &mut debug).unwrap();
        let silent = rx.try_recv().unwrap();
        assert_eq!(silent.media_position, 2880);
        let mut clean = transmit::Encoder::new(true).unwrap();
        let mut expected = [0; 4000];
        let (len, _, _) = clean.encode(&[0.0; FRAME], false, &mut expected).unwrap();
        assert_eq!(&silent.payload[..], &expected[..len]);
        assert!(!s.encoder_active);
    }

    #[test]
    fn buffered_capture_cannot_be_relabelled_as_fresh_speech_after_unmute() {
        let mut s = session();
        s.connection = Some(connection::Connection::default());
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        s.outgoing = Some(tx);
        let at = Instant::now();
        s.transmit_control.invalidate(); // mute then unmute, before processing PCM
        let generation = s.transmit_control.generation();
        let captured = processing::Captured {
            pcm: [0.1; FRAME],
            evidence: gate::Evidence([0.1; 2]),
            at: [at, at + Duration::from_millis(10)],
            gap: [false; 2],
            position: 0,
        };
        s.capture_packet(
            captured,
            generation,
            &AtomicBool::new(false),
            &mut recording::Recorder::default(),
        )
        .unwrap();
        let silent = rx.try_recv().unwrap();
        assert!(!s.encoder_active);
        let mut clean = transmit::Encoder::new(true).unwrap();
        let mut expected = [0; 4000];
        let (len, _, _) = clean.encode(&[0.0; FRAME], false, &mut expected).unwrap();
        assert_eq!(&silent.payload[..], &expected[..len]);
    }
    #[test]
    fn saved_speaker_volume_rejects_stale_slots_and_other_guilds() {
        let mut s = session();
        let guild = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let id = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        let other = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        s.volumes = Some(super::super::volumes::Store::default().profile(guild));
        s.remotes[3] = Some(super::super::Remote {
            source_epoch: 0,
            privacy_epoch: 0,
            since: Instant::now(),
            deafened: false,
            decode_stats: Default::default(),
            activity: Default::default(),
            id,
            label: "A speaker".into(),
            jitter: Default::default(),
            rate: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        s.remotes[3 + thiscord_shared::voice::ROOM_CAPACITY] = Some(super::super::Remote {
            source_epoch: 0,
            privacy_epoch: 0,
            since: Instant::now(),
            deafened: false,
            decode_stats: Default::default(),
            activity: Default::default(),
            id,
            label: "Shared audio".into(),
            jitter: Default::default(),
            rate: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        let target = thiscord_shared::audio::SpeakerVolumeTarget {
            guild_id: guild,
            account_id: id,
            shared_audio: false,
        };
        s.set_volume(3, Some(target), 0.37).unwrap();
        assert_eq!(s.volumes.as_ref().unwrap().gain(id, false), 0.37);
        assert_eq!(
            f32::from_bits(
                s.devices.writers[3 + thiscord_shared::voice::ROOM_CAPACITY]
                    .control
                    .volume
                    .load(Ordering::Relaxed)
            ),
            1.0
        );
        let shared = thiscord_shared::audio::SpeakerVolumeTarget {
            shared_audio: true,
            ..target
        };
        assert!(s.set_volume(3, Some(shared), 0.0).is_err());
        s.set_volume(3 + thiscord_shared::voice::ROOM_CAPACITY, Some(shared), 0.0)
            .unwrap();
        assert_eq!(s.volumes.as_ref().unwrap().gain(id, true), 0.0);
        let levels = s.status().streams;
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].target, Some(target));
        assert_eq!(levels[0].volume, 0.37);
        assert_eq!(levels[1].target, Some(shared));
        assert_eq!(levels[1].volume, 0.0);
        s.set_volume(3, Some(target), 0.0).unwrap();
        s.set_volume(3 + thiscord_shared::voice::ROOM_CAPACITY, Some(shared), 1.5)
            .unwrap();
        assert_eq!(s.status().streams[0].volume, 0.0);
        assert_eq!(s.status().streams[1].volume, 1.5);
        s.set_volume(3, Some(target), 0.37).unwrap();
        assert!(s.set_volume(3, None, 0.0).is_err());
        assert!(
            s.set_volume(
                3,
                Some(thiscord_shared::audio::SpeakerVolumeTarget {
                    account_id: other,
                    ..target
                }),
                0.0
            )
            .is_err()
        );
        let wrong_guild = "00000000-0000-0000-0000-000000000004".parse().unwrap();
        assert!(
            s.set_volume(
                3,
                Some(thiscord_shared::audio::SpeakerVolumeTarget {
                    guild_id: wrong_guild,
                    ..target
                }),
                0.0
            )
            .is_err()
        );
        assert_eq!(
            f32::from_bits(s.devices.writers[3].control.volume.load(Ordering::Relaxed)),
            0.37
        );
        assert!(s.set_volume(4, Some(target), 0.0).is_err());
    }

    type TestPending = (
        Pending,
        mpsc::Sender<Result<Replacement, String>>,
        mpsc::Receiver<Result<AudioStatus, String>>,
        mpsc::Receiver<Replacement>,
    );
    #[test]
    fn recovery_retains_call_and_controls_and_clears_stale_audio() {
        let mut s = session();
        let at = Instant::now();
        assert_eq!(s.media_clock.position(at, 0), 0);
        s.recovery.begin("device busy".into());
        s.settings.muted = true;
        s.settings.input = Some("saved microphone".into());
        s.settings.output = Some("unplugged headset".into());
        let transport = s.outgoing.as_ref().unwrap().clone();
        let connection = connection::Connection::default();
        s.connection = Some(connection.clone());
        let id = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        s.remotes[3] = Some(Remote {
            source_epoch: 0,
            privacy_epoch: 0,
            since: Instant::now(),
            deafened: false,
            decode_stats: Default::default(),
            activity: Default::default(),
            id,
            label: "speaker".into(),
            jitter: Default::default(),
            rate: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        s.devices.writers[3].volume(0.37).unwrap();
        s.devices.writers[3]
            .control
            .active
            .store(true, Ordering::Release);
        assert!(s.status().running);
        let mut replacement = devices(true);
        replacement.info = serde_json::json!({
            "input": {"name": "saved microphone", "fallback": false},
            "output": {"name": "System speakers", "fallback": true}
        });
        assert!(
            s.resume_devices(
                replacement,
                &AtomicBool::new(false),
                &AtomicBool::new(false),
                &AtomicBool::new(true)
            )
            .unwrap()
        );
        assert!(!s.recovery.active());
        assert!(s.outgoing.as_ref().unwrap().same_channel(&transport));
        assert!(s.connection.as_ref().unwrap().accepts(&connection));
        assert_eq!(s.remotes[3].as_ref().unwrap().id, id);
        assert_eq!(
            f32::from_bits(s.devices.writers[3].control.volume.load(Ordering::Relaxed)),
            0.37
        );
        assert!(s.devices.writers[3].control.active.load(Ordering::Acquire));
        assert!(s.devices.control.deafen.load(Ordering::Acquire));
        assert!(s.devices.control.mute.load(Ordering::Acquire));
        assert!(!s.devices.control.pressed.load(Ordering::Acquire));
        assert!(s.capture_after.is_some());
        assert_eq!(s.settings.input.as_deref(), Some("saved microphone"));
        assert_eq!(s.settings.output.as_deref(), Some("unplugged headset"));
        assert!(s.status().message.contains("output is unavailable"));
        assert!(s.status().message.contains("System speakers"));
        assert_eq!(
            s.media_clock.position(at + Duration::from_secs(2), 0),
            96000
        );
    }
    #[test]
    fn recovery_cannot_start_after_stop_or_revocation_and_play_failure_stays_pending() {
        let mut s = session();
        s.recovery.begin("busy".into());
        let no = AtomicBool::new(false);
        assert!(
            !s.resume_devices(devices(false), &AtomicBool::new(true), &no, &no)
                .unwrap()
        );
        let connection = connection::Connection::default();
        connection.close();
        s.connection = Some(connection);
        assert!(!s.resume_devices(devices(false), &no, &no, &no).unwrap());
        s.connection = None;
        assert!(s.resume_devices(devices(false), &no, &no, &no).is_err());
        assert!(s.recovery.active());
        assert!(s.outgoing.is_some());
    }
    fn pending() -> TestPending {
        let (send, result) = mpsc::channel();
        let (reply, response) = mpsc::channel();
        let (retire, retired) = mpsc::channel();
        (
            Pending {
                settings: AudioSettings {
                    output: Some("new-device".into()),
                    ..Default::default()
                },
                result,
                retire,
                reply: Some(reply),
                started: Instant::now(),
                cancelled: false,
            },
            send,
            response,
            retired,
        )
    }
    fn replacement(p: &Pending, works: bool) -> Replacement {
        Replacement {
            processing: processing::Prepared::new(&p.settings).unwrap(),
            devices: Some(devices(works)),
        }
    }
    #[test]
    fn device_swap_keeps_transport_roster_volumes_and_latest_deafen() {
        let (mut pending, send, response, retired) = pending();
        let mut old = session();
        let transport = old.outgoing.as_ref().unwrap().clone();
        let id = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        old.remotes[3] = Some(Remote {
            source_epoch: 0,
            privacy_epoch: 0,
            since: Instant::now(),
            deafened: false,
            decode_stats: Default::default(),
            activity: Default::default(),
            id,
            label: "speaker".into(),
            jitter: Default::default(),
            rate: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        old.devices.writers[3].volume(0.37).unwrap();
        old.devices.writers[3]
            .control
            .active
            .store(true, Ordering::Release);
        let mut current = Some(old);
        send.send(Ok(replacement(&pending, true))).ok().unwrap();
        assert!(pending.poll(&mut current, &mut Default::default(), true));
        let s = current.unwrap();
        assert!(s.outgoing.as_ref().unwrap().same_channel(&transport));
        assert_eq!(s.remotes[3].as_ref().unwrap().id, id);
        assert_eq!(
            f32::from_bits(s.devices.writers[3].control.volume.load(Ordering::Relaxed)),
            0.37
        );
        assert!(s.devices.writers[3].control.active.load(Ordering::Acquire));
        assert!(s.devices.control.deafen.load(Ordering::Acquire));
        assert!(s.capture_after.is_some());
        assert!(response.recv().unwrap().unwrap().running);
        assert!(
            retired
                .recv()
                .unwrap()
                .devices
                .unwrap()
                .control
                .deafen
                .load(Ordering::Acquire)
        );
    }
    #[test]
    fn failed_device_start_preserves_live_session_and_settings() {
        let (mut pending, send, response, retired) = pending();
        let mut current = Some(session());
        let control = current.as_ref().unwrap().devices.control.clone();
        send.send(Ok(replacement(&pending, false))).ok().unwrap();
        assert!(pending.poll(&mut current, &mut Default::default(), false));
        let s = current.unwrap();
        assert!(Arc::ptr_eq(&control, &s.devices.control));
        assert!(!control.deafen.load(Ordering::Acquire));
        assert!(s.settings.output.is_none());
        assert!(s.capture_after.is_none());
        assert!(response.recv().unwrap().is_err());
        assert!(retired.try_recv().is_ok());
    }
    #[test]
    fn cancelled_or_timed_out_preparation_cannot_replace_a_new_session() {
        for timeout in [false, true] {
            let (mut pending, send, response, retired) = pending();
            let next = replacement(&pending, true);
            if timeout {
                pending.started = Instant::now() - Duration::from_secs(9);
            } else {
                pending.cancel("left voice");
            }
            let mut current = Some(session());
            let control = current.as_ref().unwrap().devices.control.clone();
            assert!(!pending.poll(&mut current, &mut Default::default(), false));
            assert!(response.recv().unwrap().is_err());
            send.send(Ok(next)).ok().unwrap();
            assert!(pending.poll(&mut current, &mut Default::default(), false));
            assert!(Arc::ptr_eq(&control, &current.unwrap().devices.control));
            assert!(retired.try_recv().is_ok());
        }
    }
    #[test]
    fn preparation_error_leaves_existing_audio_running() {
        let (mut pending, send, response, _) = pending();
        let mut current = Some(session());
        send.send(Err("model load failed".into())).ok().unwrap();
        assert!(pending.poll(&mut current, &mut Default::default(), false));
        assert!(current.unwrap().settings.output.is_none());
        assert_eq!(response.recv().unwrap().unwrap_err(), "model load failed");
    }
    #[test]
    fn background_preparation_returns_movable_echo_state_without_opening_devices() {
        let pending = Pending::start(AudioSettings::default(), false, true, None).unwrap();
        let result = pending
            .result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .ok()
            .unwrap();
        assert!(result.devices.is_none());
        pending.retire.send(result).ok().unwrap();
    }
}
