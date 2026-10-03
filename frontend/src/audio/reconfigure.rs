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
                std::mem::swap(&mut s.devices, next);
            } else {
                debug.settings(&self.settings);
            }
            prepared.processing = s.processing.install(prepared.processing)?;
            s.hold = 0;
            s.processed_level = 0.0;
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
            volumes: None,
            cues: Default::default(),
            connection: None,
            settings: AudioSettings::default(),
            devices: devices(true),
            encoder: opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
                .unwrap(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
            microphone: true,
            phase: 0.0,
            next: Instant::now(),
            started: Instant::now(),
            hold: 100,
            outgoing: Some(tokio::sync::mpsc::channel(2).0),
            remotes: (0..MAX_STREAMS).map(|_| None).collect(),
            playback: Instant::now(),
            processed_level: 0.5,
            processing: processing::Processing::new(&AudioSettings::default()).unwrap(),
        }
    }
    #[test]
    fn saved_speaker_volume_rejects_stale_slots_and_other_guilds() {
        let mut s = session();
        let guild = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let id = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        let other = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        s.volumes = Some(super::super::volumes::Store::default().profile(guild));
        s.remotes[3] = Some(super::super::Remote {
            id,
            label: "A speaker".into(),
            jitter: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        s.remotes[11] = Some(super::super::Remote {
            id,
            label: "Shared audio".into(),
            jitter: Default::default(),
            decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        });
        let target = thiscord_shared::audio::SpeakerVolumeTarget {
            guild_id: guild,
            account_id: id,
        };
        s.set_volume(3, Some(target), 0.37).unwrap();
        assert_eq!(s.volumes.as_ref().unwrap().gain(id), 0.37);
        assert_eq!(
            f32::from_bits(s.devices.writers[11].control.volume.load(Ordering::Relaxed)),
            0.37
        );
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
            id,
            label: "speaker".into(),
            jitter: Default::default(),
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
        assert!(s.outgoing.unwrap().same_channel(&transport));
        assert_eq!(s.remotes[3].as_ref().unwrap().id, id);
        assert_eq!(
            f32::from_bits(s.devices.writers[3].control.volume.load(Ordering::Relaxed)),
            0.37
        );
        assert!(s.devices.writers[3].control.active.load(Ordering::Acquire));
        assert!(s.devices.control.deafen.load(Ordering::Acquire));
        assert_eq!(s.hold, 0);
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
        assert_eq!(s.hold, 100);
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
