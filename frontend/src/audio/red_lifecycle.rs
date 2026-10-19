//! Per-slot receive fences survive a restricted/absent Remote. Only a roster
//! establishes a source; authenticated RTP can advance its privacy revision.
use super::*;
use thiscord_shared::{AccountId, voice::Participant};

#[derive(Clone, Copy)]
pub(super) struct ReceiveFence {
    account: AccountId,
    source: u32,
    // Authoritative roster/source revision. RTP's effective revision includes
    // a receiver floor and must never overwrite this independent high-water.
    privacy: u32,
    // RTP effective privacy survives Remote removal independently of the roster.
    effective: u32,
    present: bool,
    allowed: bool,
}

fn newer(next: u32, current: u32) -> bool {
    next > current
}

impl Session {
    /// Shared by the actual worker Command path and device-free lifecycle tests.
    pub(super) fn receive_command(
        &mut self,
        command: Command,
        deafened: bool,
        now: Instant,
    ) -> Result<(), String> {
        match command {
            Command::Packet {
                payload_type,
                source_epoch,
                privacy_epoch,
                marker,
                connection,
                slot,
                sequence,
                timestamp,
                received_at,
                payload,
            } => {
                if !self
                    .connection
                    .as_ref()
                    .is_some_and(|c| c.accepts(&connection))
                    || deafened
                    || self.settings.deafened
                    || self.devices.control.deafen.load(Ordering::Acquire)
                    || (slot < thiscord_shared::voice::ROOM_CAPACITY
                        && !self.transmit_control.receive_allowed())
                    || (slot < thiscord_shared::voice::ROOM_CAPACITY
                        && payload.len() > thiscord_shared::rtp_red::MAX_PAYLOAD)
                    || now.saturating_duration_since(received_at) >= jitter::MAX_AGE
                {
                    return Ok(());
                }
                let Some(Some(remote)) = self.remotes.get_mut(slot) else {
                    return Ok(());
                };
                if received_at < remote.since || remote.deafened {
                    return Ok(());
                }
                match (remote.source_epoch, source_epoch, privacy_epoch) {
                    // Legacy publishers have no network privacy fence. Preserve
                    // ordinary raw Opus interoperability after local pause/flush;
                    // they are outside the strict two-hop epoch guarantee and
                    // must never introduce RED without negotiated source epochs.
                    (0, None, None)
                        if payload_type == thiscord_shared::rtp_red::OPUS_PAYLOAD_TYPE => {}
                    (source, Some(incoming), Some(privacy))
                        if source != 0
                            && incoming == source
                            && privacy != 0
                            && privacy >= self.receive_floor =>
                    {
                        if privacy != remote.privacy_epoch {
                            if !newer(privacy, remote.privacy_epoch) {
                                return Ok(());
                            }
                            remote.clear(&self.devices.writers[slot], received_at);
                            remote.privacy_epoch = privacy;
                        }
                        if let Some(fence) = &mut self.receive_fences[slot] {
                            fence.effective = fence.effective.max(privacy);
                        }
                    }
                    _ => return Ok(()),
                }
                if remote.push_transport(
                    payload_type,
                    sequence,
                    timestamp,
                    payload,
                    received_at,
                    now,
                ) {
                    remote.jitter.packet_flags(timestamp, marker, false);
                }
            }
            Command::Roster {
                connection,
                members,
            } => {
                if self
                    .connection
                    .as_ref()
                    .is_some_and(|c| c.accepts(&connection))
                {
                    self.receive_roster(&members, now)?;
                }
            }
            Command::ReceiveBoundary {
                connection,
                generation,
                epoch,
            } => {
                if !self
                    .connection
                    .as_ref()
                    .is_some_and(|c| c.accepts(&connection))
                    || generation != self.transmit_control.generation()
                {
                    return Ok(());
                }
                if let Some(epoch) = epoch {
                    if epoch == 0 || epoch < self.receive_floor {
                        return Ok(());
                    }
                    if epoch == self.receive_floor && self.transmit_control.receive_allowed() {
                        return Ok(());
                    }
                }
                self.transmit_control.pause_receive();
                self.transmit_control.enable_receive_protection();
                for slot in 0..thiscord_shared::voice::ROOM_CAPACITY {
                    if let Some(remote) = &mut self.remotes[slot] {
                        remote.clear(&self.devices.writers[slot], now);
                    }
                }
                if let Some(epoch) = epoch {
                    self.receive_floor = epoch;
                    let _ = self.transmit_control.admit_receive(generation);
                }
            }
            _ => unreachable!("Only media/roster commands enter receiver admission"),
        }
        Ok(())
    }

    fn receive_roster(&mut self, members: &[Participant], now: Instant) -> Result<(), String> {
        for slot in 0..MAX_STREAMS {
            let shared_audio = slot >= thiscord_shared::voice::ROOM_CAPACITY;
            let member = members.iter().find(|m| {
                if shared_audio {
                    m.sharing_audio
                        && m.slot.checked_add(thiscord_shared::voice::ROOM_CAPACITY) == Some(slot)
                } else {
                    m.slot == slot
                }
            });
            let Some(member) = member else {
                if let Some(fence) = &mut self.receive_fences[slot] {
                    fence.present = false;
                    fence.allowed = false;
                }
                self.remove_remote(slot);
                continue;
            };
            let source = if shared_audio {
                0
            } else {
                member.microphone_source_epoch
            };
            let privacy = if shared_audio {
                0
            } else {
                member.microphone_epoch
            };
            if !shared_audio && (source == 0) != (privacy == 0) {
                continue;
            }
            let allowed = shared_audio || (member.can_speak && !member.muted && !member.deafened);
            if let Some(old) = self.receive_fences[slot] {
                if source != old.source {
                    // Sources increase monotonically within a server session;
                    // exhaustion is fail-closed, rather than a wrapping source.
                    if source <= old.source {
                        continue;
                    }
                } else if source != 0
                    && (old.account != member.account_id
                        || !old.present
                        || (privacy != old.privacy && !newer(privacy, old.privacy))
                        || (privacy == old.privacy && !old.allowed && allowed))
                {
                    continue;
                }
            }
            let effective = self.receive_fences[slot]
                .filter(|old| old.source == source && old.account == member.account_id)
                .map_or(privacy, |old| old.effective.max(privacy));
            self.receive_fences[slot] = Some(ReceiveFence {
                account: member.account_id,
                source,
                privacy,
                effective,
                present: true,
                allowed,
            });
            if !allowed {
                self.remove_remote(slot);
                continue;
            }
            if let Some(remote) = &mut self.remotes[slot]
                && remote.id == member.account_id
                && remote.source_epoch == source
            {
                if remote.privacy_epoch < privacy {
                    remote.clear(&self.devices.writers[slot], now);
                    remote.privacy_epoch = privacy;
                }
                continue;
            }
            self.remove_remote(slot);
            let writer = &self.devices.writers[slot];
            writer.volume(
                self.volumes
                    .as_ref()
                    .map(|p| p.gain(member.account_id, shared_audio))
                    .unwrap_or(1.0),
            )?;
            writer.control.counts.clear();
            self.remotes[slot] = Some(Remote {
                source_epoch: source,
                privacy_epoch: if shared_audio {
                    0
                } else {
                    effective.max(self.receive_floor)
                },
                since: now,
                deafened: false,
                decode_stats: Default::default(),
                activity: Default::default(),
                id: member.account_id,
                label: if shared_audio {
                    format!("{} - shared audio", member.display_name())
                } else {
                    member.display_name().to_owned()
                },
                jitter: Default::default(),
                rate: Default::default(),
                decoder: opus::Decoder::new(RATE, opus::Channels::Mono)
                    .map_err(|e| e.to_string())?,
            });
            self.remotes[slot].as_mut().unwrap().set_deep_redundancy(
                writer,
                self.settings.deep_redundancy && !shared_audio,
                now,
            )?;
            writer.control.active.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn remove_remote(&mut self, slot: usize) {
        if self.remotes[slot].take().is_some() {
            let writer = &self.devices.writers[slot];
            writer.control.active.store(false, Ordering::Release);
            writer.discard();
            writer.control.pending_media.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod dred_lifecycle {
        include!("dred/lifecycle_tests.rs");
    }

    struct FakeStreams;
    impl DeviceStreams for FakeStreams {
        fn play(&self) -> Result<(), String> {
            Ok(())
        }
    }
    fn session() -> (Session, Mixer) {
        let control = Arc::new(Controls::default());
        let transmit_control = Arc::new(outgoing::Control::default());
        let _ = control.receive_guard.set(transmit_control.clone());
        let (writers, mixer) = mixer(control.clone());
        let settings = AudioSettings {
            echo_cancellation: false,
            noise_suppression: false,
            automatic_gain: false,
            neural_echo: false,
            ..Default::default()
        };
        (
            Session {
                device_watch: Default::default(),
                recovery: Default::default(),
                volumes: None,
                connection: Some(Default::default()),
                settings: settings.clone(),
                devices: Devices {
                    info: serde_json::json!({"fake":true}),
                    streams: Box::new(FakeStreams),
                    capture: frames::queue().1,
                    reference: frames::queue().1,
                    writers,
                    control,
                    health: Arc::new(StreamHealth::default()),
                },
                encoder: transmit::Encoder::new(true).unwrap(),
                decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
                microphone: false,
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
                transmit_control,
                encoder_generation: 0,
                encoder_active: false,
                remotes: (0..MAX_STREAMS).map(|_| None).collect(),
                receive_fences: Default::default(),
                receive_floor: 0,
                processed_level: 0.0,
                processing: processing::Processing::new(&settings).unwrap(),
            },
            mixer,
        )
    }
    fn member(source: u32, privacy: u32) -> Participant {
        serde_json::from_value(serde_json::json!({
            "account_id":"00000000-0000-0000-0000-000000000001", "username":"fixture", "slot":0,
            "microphone_source_epoch":source, "microphone_epoch":privacy,
            "muted":false,"deafened":false,"can_speak":true
        }))
        .unwrap()
    }
    fn roster(s: &mut Session, members: Vec<Participant>, now: Instant) {
        s.receive_command(
            Command::Roster {
                connection: s.connection.clone().unwrap(),
                members,
            },
            false,
            now,
        )
        .unwrap();
    }
    fn payload() -> bytes::Bytes {
        let mut encoder =
            opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let pcm: [f32; FRAME] = std::array::from_fn(|i| (i as f32 * 0.05).sin() * 0.1);
        let mut output = [0; 1500];
        let len = encoder.encode_float(&pcm, &mut output).unwrap();
        bytes::Bytes::copy_from_slice(&output[..len])
    }
    fn packet(
        s: &Session,
        source: Option<u32>,
        privacy: Option<u32>,
        received_at: Instant,
    ) -> Command {
        Command::Packet {
            payload_type: 111,
            source_epoch: source,
            privacy_epoch: privacy,
            marker: false,
            connection: s.connection.clone().unwrap(),
            slot: 0,
            sequence: 1,
            timestamp: 960,
            received_at,
            payload: payload(),
        }
    }
    fn dispatch(
        s: &mut Session,
        source: Option<u32>,
        privacy: Option<u32>,
        received_at: Instant,
        now: Instant,
    ) {
        let command = packet(s, source, privacy, received_at);
        s.receive_command(command, false, now).unwrap();
    }
    fn generation(s: &Session) -> u64 {
        s.devices.writers[0]
            .control
            .generation
            .load(Ordering::Acquire)
    }

    #[test]
    fn packet_epoch_flushes_pcm_and_jitter_before_roster_arrives() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 2)], at);
        dispatch(&mut s, Some(10), Some(2), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        s.remotes[0]
            .as_mut()
            .unwrap()
            .activity
            .observe(&[0.2; 960], at);
        assert_eq!(s.devices.writers[0].write(&[0.5; 960]), 960);
        let before = generation(&s);
        let next = at + Duration::from_millis(20);
        dispatch(&mut s, Some(10), Some(3), next, next);
        assert!(generation(&s) > before);
        let remote = s.remotes[0].as_ref().unwrap();
        assert_eq!(remote.privacy_epoch, 3);
        assert_eq!(remote.since, next);
        assert!(!remote.activity.speaking(next));
        let mut output = [1.0; 960];
        mixer.render(&mut output, 1);
        assert!(output.iter().all(|sample| *sample == 0.0));
        assert_eq!(s.devices.writers[0].occupied_samples(), 0);
        let before = generation(&s);
        let mut stale_restriction = member(10, 1);
        stale_restriction.muted = true;
        roster(&mut s, vec![stale_restriction], next);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 3);
        assert_eq!(generation(&s), before);
        dispatch(&mut s, Some(10), Some(1), next, next);
        assert_eq!(generation(&s), before);
    }

    #[test]
    fn effective_high_water_survives_restriction_and_stale_roster_recreation() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 10)], at);
        dispatch(&mut s, Some(10), Some(100), at, at);
        s.devices.writers[0].write(&[0.5; 960]);
        let mut restricted = member(10, 20);
        restricted.muted = true;
        roster(&mut s, vec![restricted], at);
        assert!(s.remotes[0].is_none());
        assert_eq!(s.receive_fences[0].unwrap().effective, 100);
        roster(&mut s, vec![member(10, 30)], at);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 100);
        dispatch(&mut s, Some(10), Some(30), at, at);
        let mut stale_red = packet(&s, Some(10), Some(30), at);
        if let Command::Packet {
            payload_type,
            payload,
            ..
        } = &mut stale_red
        {
            *payload_type = thiscord_shared::rtp_red::PAYLOAD_TYPE;
            // Structurally valid history must fail before donor admission.
            *payload = thiscord_shared::rtp_red::encode(
                &[0xf8, 0xff, 0xfe],
                &[thiscord_shared::rtp_red::Block {
                    offset: 960,
                    payload: &[0xf8, 0xff, 0xfe],
                }],
            )
            .unwrap()
            .into();
        }
        s.receive_command(stale_red, false, at).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        let mut output = [1.0; 960];
        mixer.render(&mut output, 1);
        assert!(output.iter().all(|sample| *sample == 0.0));
        dispatch(&mut s, Some(10), Some(100), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        // A new authoritative source starts a distinct privacy history.
        roster(&mut s, vec![member(101, 101)], at);
        assert_eq!(s.receive_fences[0].unwrap().effective, 101);
    }

    #[test]
    fn queued_partial_wrong_source_and_expired_packets_cannot_advance_fence() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 5)], at);
        let before = generation(&s);
        for (source, privacy, received, now) in [
            (Some(10), Some(6), at - Duration::from_millis(1), at),
            (Some(10), Some(6), at, at + jitter::MAX_AGE),
            (Some(10), None, at, at),
            (None, Some(6), at, at),
            (None, None, at, at),
            (Some(10), Some(0), at, at),
            (Some(11), Some(6), at, at),
            (Some(9), Some(6), at, at),
            (Some(10), Some(4), at, at),
        ] {
            dispatch(&mut s, source, privacy, received, now);
            assert_eq!(generation(&s), before);
            assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 5);
            assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        }
        let mut old_connection = packet(&s, Some(10), Some(6), at);
        if let Command::Packet { connection, .. } = &mut old_connection {
            *connection = Default::default();
        }
        s.receive_command(old_connection, false, at).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 5);
    }

    #[test]
    fn restrictions_tombstones_and_same_account_source_replacement_never_roll_back() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 1)], at);
        for revision in 2..=4 {
            s.devices.writers[0].write(&[0.5; 960]);
            let mut restricted = member(10, revision * 2);
            if revision == 2 {
                restricted.muted = true;
            }
            if revision == 3 {
                restricted.deafened = true;
            }
            if revision == 4 {
                restricted.can_speak = false;
            }
            roster(&mut s, vec![restricted], at);
            assert!(s.remotes[0].is_none());
            roster(&mut s, vec![member(10, revision * 2 - 1)], at);
            roster(&mut s, vec![member(10, revision * 2)], at);
            dispatch(&mut s, Some(10), Some(revision * 2 + 1), at, at);
            assert!(s.remotes[0].is_none());
            let mut output = [1.0; 960];
            mixer.render(&mut output, 1);
            assert!(output.iter().all(|v| *v == 0.0));
            roster(&mut s, vec![member(10, revision * 2 + 1)], at);
            assert!(s.remotes[0].is_some());
        }
        let queued = packet(&s, Some(10), Some(20), at);
        roster(&mut s, vec![member(11, 1)], at);
        s.receive_command(queued, false, at).unwrap();
        roster(&mut s, vec![member(10, 20)], at);
        assert_eq!(s.remotes[0].as_ref().unwrap().source_epoch, 11);
        roster(&mut s, vec![], at);
        roster(&mut s, vec![member(11, 20)], at);
        assert!(s.remotes[0].is_none());
        let mut replacement = member(12, 1);
        replacement.account_id = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        roster(&mut s, vec![replacement], at);
        assert_eq!(s.remotes[0].as_ref().unwrap().source_epoch, 12);
    }

    #[test]
    fn roster_only_revision_change_flushes_and_epoch_exhaustion_cannot_wrap() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, u32::MAX - 1)], at);
        dispatch(&mut s, Some(10), Some(u32::MAX - 1), at, at);
        let before = generation(&s);
        roster(&mut s, vec![member(10, u32::MAX)], at);
        assert!(generation(&s) > before);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        dispatch(&mut s, Some(10), Some(1), at, at);
        dispatch(&mut s, Some(10), Some(0), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, u32::MAX);
    }

    #[test]
    fn legacy_requires_no_epochs_and_shared_audio_keeps_its_existing_policy() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(0, 0)], at);
        dispatch(&mut s, Some(0), Some(0), at, at);
        dispatch(&mut s, None, Some(1), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        dispatch(&mut s, None, None, at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        let mut shared = member(10, 1);
        shared.muted = true;
        shared.can_speak = false;
        shared.sharing_audio = true;
        roster(&mut s, vec![shared], at);
        assert!(s.remotes[0].is_none());
        assert!(s.remotes[thiscord_shared::voice::ROOM_CAPACITY].is_some());
    }

    #[test]
    fn modern_receiver_boundary_preserves_legacy_raw_opus_but_rejects_red() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(0, 0)], at);
        let current = s.transmit_control.generation();
        boundary(&mut s, current, None, at);
        dispatch(&mut s, None, None, at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        boundary(&mut s, current, Some(11), at);
        assert_eq!(s.receive_floor, 11);
        let mut red_packet = packet(&s, None, None, at);
        if let Command::Packet {
            payload_type,
            payload,
            ..
        } = &mut red_packet
        {
            *payload_type = thiscord_shared::rtp_red::PAYLOAD_TYPE;
            *payload = thiscord_shared::rtp_red::encode(payload, &[])
                .unwrap()
                .into();
        }
        s.receive_command(red_packet, false, at).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        dispatch(&mut s, None, None, at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
    }

    #[test]
    fn modern_to_legacy_rejoin_resets_attempt_state_once_and_restores_raw_audio() {
        let at = Instant::now();
        let legacy = member(0, 0);
        let (old_packet, old_ack, old_connection, mut rejoin_errors) = {
            let (mut old, _) = session();
            let modern = member(10, 7);
            let mut observer = red::RosterCompatibility::default();
            observer.observe(std::slice::from_ref(&modern)).unwrap();
            roster(&mut old, vec![modern], at);
            let generation = old.transmit_control.generation();
            let mut exchange = red::boundary::Boundary::default();
            exchange.control(generation).unwrap();
            let (tag, challenge) = exchange.request(at).unwrap().unwrap();
            assert!(exchange.acknowledge(tag, challenge, true));
            boundary(&mut old, generation, Some(11), at);
            assert_eq!(old.receive_floor, 11);
            let old_packet = packet(&old, Some(10), Some(11), at);

            // Leave an actual refresh challenge pending in the old attempt.
            exchange.require_refresh(99);
            old.transmit_control.invalidate();
            exchange
                .start_refresh(old.transmit_control.generation())
                .unwrap();
            let pending = exchange.request(at).unwrap().unwrap();
            assert_eq!(pending.1, Some(99));
            assert!(!old.transmit_control.receive_allowed());
            assert!(!exchange.ready());
            observer.observe(&[]).unwrap();
            roster(&mut old, vec![], at);
            assert!(old.receive_fences[0].is_some());
            let error = observer.observe(std::slice::from_ref(&legacy)).unwrap_err();
            assert!(error.contains("Rejoin voice"));
            // Even if an old worker command remains queued, preserve the source
            // tombstone. Compatibility is restored by a new attempt, not reset.
            roster(&mut old, vec![legacy.clone()], at);
            assert!(old.remotes[0].is_none());
            let connection = old.connection.clone().unwrap();
            connection.close();
            // Scope exit models native attempt cleanup, dropping Session and
            // all per-attempt signaling/roster state before the explicit rejoin.
            (old_packet, pending, connection, 1_usize)
        };
        assert!(!old_connection.active());

        let (mut fresh, mut mixer) = session();
        let mut observer = red::RosterCompatibility::default();
        let mut exchange = red::boundary::Boundary::default();
        assert_eq!(fresh.receive_floor, 0);
        assert!(fresh.receive_fences.iter().all(Option::is_none));
        assert!(fresh.transmit_control.receive_allowed());
        assert!(
            exchange
                .request(at + Duration::from_secs(10))
                .unwrap()
                .is_none()
        );
        assert!(!exchange.acknowledge(old_ack.0, old_ack.1, true));
        assert!(!exchange.needs_refresh());
        let generation = fresh.transmit_control.generation();
        exchange.control(generation).unwrap();
        let current = exchange.request(at).unwrap().unwrap();
        assert_eq!(current, (1, None));
        assert!(exchange.acknowledge(current.0, current.1, true));
        assert!(exchange.ready_control(generation));
        assert!(!exchange.needs_refresh());
        boundary(&mut fresh, generation, Some(3), at);
        assert_eq!(fresh.receive_floor, 3);
        // Repeated ordinary rosters on the new attempt do not request another
        // rejoin, even though this slot previously held a modern publisher.
        for _ in 0..2 {
            rejoin_errors += usize::from(observer.observe(std::slice::from_ref(&legacy)).is_err());
            roster(&mut fresh, vec![legacy.clone()], at);
        }
        assert_eq!(rejoin_errors, 1);
        assert_eq!(fresh.remotes[0].as_ref().unwrap().source_epoch, 0);
        fresh.receive_command(old_packet, false, at).unwrap();
        assert_eq!(
            fresh.remotes[0].as_ref().unwrap().jitter.metrics().packets,
            0
        );
        dispatch(&mut fresh, None, None, at, at);
        assert_eq!(
            fresh.remotes[0].as_ref().unwrap().jitter.metrics().packets,
            1
        );
        let mut energy = 0.0;
        for tick in 0..8 {
            let now = at + Duration::from_millis(60 + tick * 10);
            fresh.remotes[0]
                .as_mut()
                .unwrap()
                .refill(&mut fresh.devices.writers[0], now);
            let mut output = [0.0_f32; 480];
            mixer.render_at(&mut output, 1, now);
            energy += output.iter().map(|sample| sample * sample).sum::<f32>();
        }
        assert!(
            energy > 0.001,
            "fresh legacy Opus must reach actual playback"
        );
        assert_eq!(rejoin_errors, 1);
    }

    #[test]
    fn delayed_refresh_ready_produces_pcm_within_service_window_without_moving_receive_floor() {
        let (mut listener, mut mixer) = session();
        let at = Instant::now();
        roster(&mut listener, vec![member(10, 7)], at);
        let original_control = listener.transmit_control.generation();
        let mut exchange = red::boundary::Boundary::default();
        exchange.control(original_control).unwrap();
        let initial = exchange.request(at).unwrap().unwrap();
        assert!(exchange.acknowledge(initial.0, initial.1, true));
        boundary(&mut listener, original_control, Some(11), at);
        dispatch(&mut listener, Some(10), Some(11), at, at);
        assert_eq!(
            listener.remotes[0]
                .as_ref()
                .unwrap()
                .jitter
                .metrics()
                .packets,
            1
        );

        listener.transmit_control.invalidate();
        exchange.require_refresh(50);
        exchange
            .start_refresh(listener.transmit_control.generation())
            .unwrap();
        let superseded = exchange.request(at).unwrap().unwrap();
        // A second challenge/control change coalesces while signaling holds the
        // first ACK. Receive admission remains closed during both delays.
        listener.transmit_control.invalidate();
        let current_control = listener.transmit_control.generation();
        exchange.require_refresh(51);
        exchange.start_refresh(current_control).unwrap();
        boundary(&mut listener, current_control, None, at);
        assert!(exchange.ready_notice().is_none());
        let first_ack_at = at + Duration::from_millis(400);
        assert!(!exchange.acknowledge(superseded.0, superseded.1, true));
        let latest = exchange.request(first_ack_at).unwrap().unwrap();
        assert_eq!(latest.1, Some(51));
        for now in [first_ack_at, at + Duration::from_millis(699)] {
            dispatch(&mut listener, Some(10), Some(11), now, now);
            assert!(!listener.transmit_control.receive_allowed());
            assert!(!exchange.ready_control(current_control));
            assert!(exchange.ready_notice().is_none());
            listener.remotes[0]
                .as_mut()
                .unwrap()
                .refill(&mut listener.devices.writers[0], now);
            let mut output = [0.0_f32; 480];
            mixer.render_at(&mut output, 1, now);
            assert!(output.iter().all(|sample| *sample == 0.0));
            assert_eq!(listener.devices.writers[0].occupied_samples(), 0);
        }
        let ready_at = at + Duration::from_millis(700);
        assert!(exchange.acknowledge(latest.0, latest.1, true));
        assert!(exchange.ready_control(current_control));
        // Source-only proof ACK keeps the listener's prior barrier. Its source
        // privacy revision (50) is independent of that unchanged floor (11).
        boundary(&mut listener, current_control, Some(11), ready_at);
        assert_eq!(listener.receive_floor, 11);
        assert!(listener.transmit_control.receive_allowed());
        let (generation, challenge) = exchange.ready_notice().unwrap();
        let ready = thiscord_shared::voice::ClientEvent::MicrophoneRefreshReady {
            generation,
            challenge,
        };
        assert!(
            matches!(ready, thiscord_shared::voice::ClientEvent::MicrophoneRefreshReady { generation, challenge: 51 } if generation == latest.0)
        );
        assert!(exchange.ready_notice().is_none());

        let (mut newer, mut newer_mixer) = session();
        roster(&mut newer, vec![member(10, 7)], ready_at);
        let newer_control = newer.transmit_control.generation();
        boundary(&mut newer, newer_control, Some(60), ready_at);
        let encoded = payload();
        let mut energy = 0.0_f32;
        let mut newer_energy = 0.0_f32;
        let mut first_audible = None;
        // Five fresh 20 ms packets. No artificial PCM, extra jitter cushion or
        // wall-clock sleeps: the production worker/decoder/mixer must become
        // audible inside the server's separately tested 250 ms Ready window.
        for tick in 0..=20 {
            let now = ready_at + Duration::from_millis(tick * 10);
            if tick <= 8 && tick % 2 == 0 {
                for receiver in [&mut listener, &mut newer] {
                    let mut fresh = packet(receiver, Some(10), Some(50), now);
                    if let Command::Packet {
                        sequence,
                        timestamp,
                        payload,
                        ..
                    } = &mut fresh
                    {
                        *sequence = 100 + tick as u16 / 2;
                        *timestamp = 48_000 + tick as u32 * 480;
                        *payload = encoded.clone();
                    }
                    receiver.receive_command(fresh, false, now).unwrap();
                    let before = receiver.remotes[0]
                        .as_ref()
                        .unwrap()
                        .jitter
                        .metrics()
                        .packets;
                    let mut stale = packet(receiver, Some(10), Some(11), now);
                    if let Command::Packet {
                        sequence,
                        timestamp,
                        ..
                    } = &mut stale
                    {
                        *sequence = 200 + tick as u16;
                        *timestamp = 48_000 + tick as u32 * 480;
                    }
                    receiver.receive_command(stale, false, now).unwrap();
                    // The first receiver's old epoch is also below its latest
                    // source epoch after the initial fresh primary is admitted.
                    assert_eq!(
                        receiver.remotes[0]
                            .as_ref()
                            .unwrap()
                            .jitter
                            .metrics()
                            .packets,
                        before
                    );
                }
            }
            listener.remotes[0]
                .as_mut()
                .unwrap()
                .refill(&mut listener.devices.writers[0], now);
            newer.remotes[0]
                .as_mut()
                .unwrap()
                .refill(&mut newer.devices.writers[0], now);
            let mut output = [0.0_f32; 480];
            mixer.render_at(&mut output, 1, now);
            let frame_energy = output.iter().map(|sample| sample * sample).sum::<f32>();
            energy += frame_energy;
            if frame_energy > 0.001 {
                first_audible.get_or_insert(now);
            }
            newer_mixer.render_at(&mut output, 1, now);
            newer_energy += output.iter().map(|sample| sample * sample).sum::<f32>();
        }
        assert!(energy > 0.001);
        assert!(first_audible.unwrap().duration_since(ready_at) < Duration::from_millis(250));
        assert_eq!(listener.receive_floor, 11);
        assert_eq!(listener.remotes[0].as_ref().unwrap().privacy_epoch, 50);
        assert_eq!(newer_energy, 0.0);
        assert_eq!(
            newer.remotes[0]
                .as_ref()
                .unwrap()
                .decode_stats
                .normal_frames,
            0
        );
        assert_eq!(
            newer.remotes[0].as_ref().unwrap().jitter.metrics().packets,
            0
        );
        let next_proof = ready_at + Duration::from_millis(220);
        dispatch(&mut newer, Some(10), Some(61), next_proof, next_proof);
        assert_eq!(newer.remotes[0].as_ref().unwrap().privacy_epoch, 61);
        assert_eq!(
            newer.remotes[0].as_ref().unwrap().jitter.metrics().packets,
            1
        );
        assert_eq!(newer.receive_floor, 60);
    }

    #[test]
    fn local_deafen_and_rapid_privacy_boundaries_do_not_replay_queued_audio() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 1)], at);
        let queued = packet(&s, Some(10), Some(4), at);
        s.devices.writers[0].write(&[0.5; 960]);
        let disabled = at + Duration::from_millis(20);
        s.remotes[0]
            .as_mut()
            .unwrap()
            .set_deafened(&s.devices.writers[0], true, disabled);
        s.settings.deafened = true;
        dispatch(&mut s, Some(10), Some(2), disabled, disabled);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 1);
        let resumed = at + Duration::from_millis(40);
        s.settings.deafened = false;
        s.remotes[0]
            .as_mut()
            .unwrap()
            .set_deafened(&s.devices.writers[0], false, resumed);
        s.receive_command(queued, false, resumed).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 1);
        let mut output = [1.0; 960];
        mixer.render(&mut output, 1);
        assert!(output.iter().all(|sample| *sample == 0.0));
        // Release/repress/settings boundaries can advance repeatedly without a
        // roster interval. Each forward revision invalidates the callback ring.
        for privacy in 2..=4 {
            let now = resumed + Duration::from_millis(privacy as u64 * 20);
            let before = generation(&s);
            let mut next = packet(&s, Some(10), Some(privacy), now);
            if let Command::Packet {
                sequence,
                timestamp,
                ..
            } = &mut next
            {
                // Privacy resets preserve the anti-replay RTP watermark. Real
                // senders continue their clock across release/settings changes.
                *sequence = privacy as u16;
                *timestamp = privacy * 960;
            }
            s.receive_command(next, false, now).unwrap();
            assert!(generation(&s) > before);
            assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, privacy);
            assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        }
        let urgent = packet(&s, Some(10), Some(5), resumed + Duration::from_millis(100));
        s.receive_command(urgent, true, resumed + Duration::from_millis(100))
            .unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 4);
    }

    #[test]
    fn untrusted_red_boundary_primaries_cannot_retain_donors_or_decode_pcm() {
        use thiscord_shared::rtp_red as wire;
        let mut encoder = codec::microphone_encoder().unwrap();
        encoder.set_vbr(false).unwrap();
        let pcm: [f32; FRAME] = std::array::from_fn(|i| (i as f32 * 0.05).sin() * 0.2);
        let mut encoded = [0; 1500];
        let n = encoder.encode_float(&pcm, &mut encoded).unwrap();
        assert_eq!(n, 80);
        let donor = encoded[..n].to_vec();
        let mut padded = donor.clone();
        padded.resize(100, 0);
        opus::packet::pad(&mut padded, donor.len()).unwrap();
        encoder.set_bitrate(opus::Bitrate::Bits(96_000)).unwrap();
        let n = encoder.encode_float(&pcm, &mut encoded).unwrap();
        let large = encoded[..n].to_vec();
        assert!(large.len() > wire::MAX_BLOCK);
        let dtx = vec![0x78, 0];
        for primary in [padded, dtx, large] {
            assert_eq!(opus::packet::get_nb_samples(&primary, RATE).unwrap(), FRAME);
            assert!(!wire::repeatable(&primary));
            let (mut s, mut mixer) = session();
            let at = Instant::now();
            roster(&mut s, vec![member(10, 1)], at);
            let mut first = packet(&s, Some(10), Some(1), at);
            if let Command::Packet { payload, .. } = &mut first {
                *payload = donor.clone().into();
            }
            s.receive_command(first, false, at).unwrap();
            // Deliberately bypass the safe encoder: construct an otherwise
            // correct RFC2198 header, one 20 ms donor, and forbidden primary.
            let mut raw = vec![
                wire::OPUS_PAYLOAD_TYPE | 0x80,
                15,
                0,
                donor.len() as u8,
                wire::OPUS_PAYLOAD_TYPE,
            ];
            raw.extend_from_slice(&donor);
            raw.extend_from_slice(&primary);
            assert!(raw.len() < wire::MAX_PAYLOAD);
            let mut malicious = packet(&s, Some(10), Some(1), at);
            if let Command::Packet {
                payload_type,
                sequence,
                timestamp,
                payload,
                ..
            } = &mut malicious
            {
                *payload_type = wire::PAYLOAD_TYPE;
                *sequence = 3;
                *timestamp = 2880;
                *payload = raw.into();
            }
            s.receive_command(malicious, false, at).unwrap();
            let remote = s.remotes[0].as_mut().unwrap();
            assert_eq!(remote.jitter.metrics().packets, 1);
            let due = at + Duration::from_millis(60);
            assert!(matches!(
                remote.jitter.pop(true, due),
                Some(jitter::Playout::Packet(_))
            ));
            // Neither the 1920 donor nor the 2880 primary can become a normal
            // decode. No redundant history was retained behind the parser.
            for _ in 0..2 {
                assert!(!matches!(
                    remote.jitter.pop(true, due),
                    Some(jitter::Playout::Packet(_))
                ));
            }
            assert_eq!(s.devices.writers[0].occupied_samples(), 0);
            let mut output = [1.0; 960];
            mixer.render(&mut output, 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
        }
    }

    fn boundary(s: &mut Session, generation: u64, epoch: Option<u32>, now: Instant) {
        s.receive_command(
            Command::ReceiveBoundary {
                connection: s.connection.clone().unwrap(),
                generation,
                epoch,
            },
            false,
            now,
        )
        .unwrap();
    }

    #[test]
    fn oversized_raw_microphone_is_rejected_before_privacy_or_pcm_admission() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 7)], at);
        let mut command = packet(&s, Some(10), Some(8), at);
        if let Command::Packet { payload, .. } = &mut command {
            *payload = vec![0x78; thiscord_shared::rtp_red::MAX_PAYLOAD + 1].into();
        }
        s.receive_command(command, false, at).unwrap();
        let remote = s.remotes[0].as_ref().unwrap();
        assert_eq!(remote.privacy_epoch, 7);
        assert_eq!(remote.jitter.metrics().packets, 0);
        let mut output = [1.0; FRAME];
        mixer.render(&mut output, 1);
        assert!(output.iter().all(|sample| *sample == 0.0));
    }
    #[test]
    fn receive_boundary_holds_until_current_ack_and_rejects_old_udp() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 7)], at);
        let first = s.transmit_control.generation();
        boundary(&mut s, first, None, at);
        assert!(!s.transmit_control.receive_allowed());
        dispatch(&mut s, Some(10), Some(20), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        s.transmit_control.invalidate();
        let current = s.transmit_control.generation();
        boundary(&mut s, first, Some(9), at);
        assert!(!s.transmit_control.receive_allowed());
        assert_eq!(s.receive_floor, 0);
        boundary(&mut s, current, Some(11), at);
        assert!(s.transmit_control.receive_allowed());
        assert_eq!(s.receive_floor, 11);
        dispatch(&mut s, Some(10), Some(10), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        dispatch(&mut s, Some(10), Some(11), at, at);
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        boundary(&mut s, current, Some(10), at);
        boundary(&mut s, first, Some(12), at);
        assert_eq!(s.receive_floor, 11);
        assert!(s.transmit_control.receive_allowed());
        // The next immediate local cutoff closes reception without waiting for
        // the worker's queued None command or a delayed prior acknowledgment.
        s.transmit_control.invalidate();
        assert!(!s.transmit_control.receive_allowed());
        boundary(&mut s, current, Some(11), at);
        assert!(!s.transmit_control.receive_allowed());
        assert_eq!(s.receive_floor, 11);
    }

    #[test]
    fn effective_packet_epoch_does_not_replace_authoritative_roster_revision() {
        let (mut s, _) = session();
        let at = Instant::now();
        roster(&mut s, vec![member(10, 7)], at);
        let current = s.transmit_control.generation();
        boundary(&mut s, current, Some(11), at);
        dispatch(&mut s, Some(10), Some(11), at, at);
        let before = generation(&s);
        roster(&mut s, vec![member(10, 7)], at);
        assert_eq!(generation(&s), before);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 11);
        let mut stale = member(10, 6);
        stale.muted = true;
        roster(&mut s, vec![stale], at);
        assert!(s.remotes[0].is_some());
        let mut restriction = member(10, 8);
        restriction.muted = true;
        roster(&mut s, vec![restriction], at);
        assert!(s.remotes[0].is_none());
        roster(&mut s, vec![member(10, 7)], at);
        assert!(s.remotes[0].is_none());
        roster(&mut s, vec![member(10, 12)], at);
        assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 12);
        let mut same_revision_restriction = member(10, 12);
        same_revision_restriction.can_speak = false;
        roster(&mut s, vec![same_revision_restriction], at);
        assert!(s.remotes[0].is_none());
    }

    #[test]
    fn urgent_atomic_gate_discards_microphone_pcm_without_silencing_shared_audio() {
        let (mut s, mut mixer) = session();
        let at = Instant::now();
        let mut sharing = member(10, 1);
        sharing.sharing_audio = true;
        roster(&mut s, vec![sharing], at);
        s.transmit_control.enable_receive_protection();
        assert!(
            s.transmit_control
                .admit_receive(s.transmit_control.generation())
        );
        let shared = thiscord_shared::voice::ROOM_CAPACITY;
        s.devices.writers[0].write(&[0.5; 960]);
        s.devices.writers[0]
            .control
            .pending_media
            .store(true, Ordering::Release);
        s.devices.writers[shared].write(&[0.2; 960]);
        // This is the same atomic cutoff used by urgent settings/PTT/deafen.
        // No worker command has run yet.
        s.transmit_control.invalidate();
        let mut output = [0.0; 960];
        mixer.render(&mut output, 1);
        assert!(
            output
                .iter()
                .all(|sample| (*sample - 0.2_f32).abs() < 0.00001)
        );
        assert_eq!(s.devices.writers[0].occupied_samples(), 0);
        let metrics = quality::Metrics::default();
        s.devices.writers[0].control.counts.drain(&metrics);
        let counts = metrics.snapshot().total;
        assert_eq!(counts.intentional_silence_samples, 960);
        assert_eq!(counts.starvation_samples, 0);
        assert_eq!(counts.pcm_discarded_samples, 960);
        let current = s.transmit_control.generation();
        boundary(&mut s, current, None, at);
        let mut shared_packet = packet(&s, None, None, at);
        if let Command::Packet { slot, .. } = &mut shared_packet {
            *slot = shared;
        }
        s.receive_command(shared_packet, false, at).unwrap();
        assert_eq!(
            s.remotes[shared].as_ref().unwrap().jitter.metrics().packets,
            1
        );
    }

    #[test]
    fn saturated_or_disconnected_critical_queue_pauses_and_closes_connection() {
        let (sender, receiver) = mpsc::sync_channel(1);
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
        engine.notify(Command::Peek);
        let connection = connection::Connection::default();
        assert!(
            engine
                .notify_control(Command::Roster {
                    connection: connection.clone(),
                    members: vec![]
                })
                .is_err()
        );
        assert!(!connection.active());
        assert!(!engine.probe.outgoing.receive_allowed());
        receiver.try_recv().unwrap();
        assert!(
            engine
                .probe
                .outgoing
                .admit_receive(engine.probe.outgoing.generation())
        );
        let connection = connection::Connection::default();
        engine
            .notify_control(Command::ReceiveBoundary {
                connection: connection.clone(),
                generation: engine.probe.outgoing.generation(),
                epoch: None,
            })
            .unwrap();
        assert!(connection.active());
        assert!(!engine.probe.outgoing.receive_allowed());
        receiver.try_recv().unwrap();
        assert!(
            engine
                .probe
                .outgoing
                .admit_receive(engine.probe.outgoing.generation())
        );
        drop(receiver);
        assert!(
            engine
                .notify_control(Command::ReceiveBoundary {
                    connection: connection.clone(),
                    generation: engine.probe.outgoing.generation(),
                    epoch: Some(8),
                })
                .is_err()
        );
        assert!(!connection.active());
        assert!(!engine.probe.outgoing.receive_allowed());
    }
}
