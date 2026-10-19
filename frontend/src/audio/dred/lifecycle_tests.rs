// Exercise the production worker admission path with the RED device-free fixture.
use super::*;
use crate::audio::{dred, fixture};
use bytes::Bytes;

fn encoded() -> &'static [Bytes] {
    static PACKETS: std::sync::OnceLock<Vec<Bytes>> = std::sync::OnceLock::new();
    PACKETS.get_or_init(|| {
        let mut encoder = transmit::Encoder::new(true).unwrap();
        encoder.set_deep_redundancy(true).unwrap();
        fixture::speech()
            .as_chunks::<FRAME>()
            .0
            .iter()
            .map(|pcm| {
                let mut bytes = [0; 1500];
                let (len, omitted, _) = encoder.encode(pcm, true, &mut bytes).unwrap();
                assert!(!omitted);
                Bytes::copy_from_slice(&bytes[..len])
            })
            .collect()
    })
}

fn configured(red: bool) -> (Session, Mixer) {
    let (mut s, mixer) = session();
    s.set_deep_redundancy(true).unwrap();
    s.settings.deep_redundancy = true;
    s.settings.packet_redundancy = red;
    (s, mixer)
}

fn media(
    s: &Session,
    source: Option<u32>,
    privacy: Option<u32>,
    index: usize,
    now: Instant,
) -> Command {
    let mut command = packet(s, source, privacy, now);
    if let Command::Packet {
        sequence,
        timestamp,
        payload,
        ..
    } = &mut command
    {
        *sequence = index as u16;
        *timestamp = (index * FRAME) as u32;
        *payload = encoded()[index].clone();
    }
    command
}

fn dirty(s: &mut Session) {
    let remote = s.remotes[0].as_mut().unwrap();
    let mut pcm = [0.0; FRAME];
    for packet in &encoded()[..30] {
        remote
            .decoder
            .decode_float(packet, &mut pcm, false)
            .unwrap();
    }
    assert!(dred::decode(
        &mut remote.decoder,
        &encoded()[32],
        1920,
        &mut pcm,
        &mut remote.decode_stats
    ));
    assert_eq!(s.devices.writers[0].write(&[0.5; FRAME]), FRAME);
}

fn assert_fresh(remote: &mut Remote) {
    let mut fresh = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    fresh.set_complexity(5).unwrap();
    let mut actual = [0.0; FRAME];
    let mut expected = [0.0; FRAME];
    // Include PLC before and after normal decode to expose retained neural state.
    for packet in [&[][..], &encoded()[70], &[], &encoded()[71]] {
        remote
            .decoder
            .decode_float(packet, &mut actual, false)
            .unwrap();
        fresh.decode_float(packet, &mut expected, false).unwrap();
        assert!(
            actual == expected,
            "native state must match a fresh decoder"
        );
    }
}

fn assert_silent(mixer: &mut Mixer) {
    let mut pcm = [1.0; FRAME];
    mixer.render(&mut pcm, 1);
    assert!(pcm.iter().all(|sample| *sample == 0.0));
}

#[test]
fn dred_off_on_and_legacy_rejoin_cannot_revive_neural_state() {
    let (mut s, mut mixer) = configured(false);
    let at = Instant::now();
    roster(&mut s, vec![member(10, 10)], at);
    dirty(&mut s);
    s.set_deep_redundancy(false).unwrap();
    s.settings.deep_redundancy = false;
    assert_eq!(
        s.remotes[0]
            .as_mut()
            .unwrap()
            .decoder
            .get_complexity()
            .unwrap(),
        0
    );
    assert_silent(&mut mixer);
    s.set_deep_redundancy(true).unwrap();
    s.settings.deep_redundancy = true;
    assert_fresh(s.remotes[0].as_mut().unwrap());
    // Modern-to-legacy replacement requires a new authorized session; an old
    // epoch-zero roster cannot erase this session's source tombstone.
    roster(&mut s, vec![member(0, 0)], at);
    assert_eq!(s.remotes[0].as_ref().unwrap().source_epoch, 10);
    let old_connection = media(&s, Some(10), Some(10), 31, at);
    drop(s);
    let (mut rejoined, _) = configured(false);
    roster(&mut rejoined, vec![member(0, 0)], at);
    rejoined.receive_command(old_connection, false, at).unwrap();
    let remote = rejoined.remotes[0].as_mut().unwrap();
    assert!(!remote.jitter.deep_redundancy);
    assert_eq!(remote.decoder.get_complexity().unwrap(), 0);
    assert_eq!(remote.jitter.metrics().packets, 0);
}

#[test]
fn poisoned_decoder_cannot_fallback_until_successful_full_reset() {
    let (mut s, _) = configured(false);
    let at = Instant::now();
    roster(&mut s, vec![member(10, 10)], at);
    dirty(&mut s);
    let remote = s.remotes[0].as_mut().unwrap();
    remote.decode_stats.reset_failed = true;
    for decision in [
        jitter::Playout::Conceal(FRAME),
        jitter::Playout::Dred(encoded()[32].clone(), 1920),
        jitter::Playout::Fec(encoded()[32].clone()),
        jitter::Playout::Packet(encoded()[32].clone()),
    ] {
        let mut pcm = [1.0; FRAME];
        assert!(!codec::decode(
            &mut remote.decoder,
            &decision,
            &mut pcm,
            &mut remote.decode_stats
        ));
        assert!(pcm.iter().all(|sample| *sample == 0.0));
    }
    remote.clear(&s.devices.writers[0], at);
    assert!(!remote.decode_stats.reset_failed);
    assert_fresh(remote);
}

#[test]
fn dred_legacy_and_shared_audio_fallback_and_toggle_target_are_real() {
    for red in [false, true] {
        let (mut s, _) = configured(red);
        let at = Instant::now();
        let mut legacy = member(0, 0);
        legacy.sharing_audio = true;
        roster(&mut s, vec![legacy], at);
        let remote = s.remotes[0].as_mut().unwrap();
        assert!(!remote.jitter.deep_redundancy);
        assert_eq!(remote.decoder.get_complexity().unwrap(), 0);
        assert_eq!(remote.jitter.metrics().target_ms, 40.0);
        let command = media(&s, None, None, 30, at);
        s.receive_command(command, false, at).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
        let shared = s.remotes[thiscord_shared::voice::ROOM_CAPACITY]
            .as_mut()
            .unwrap();
        assert!(!shared.jitter.deep_redundancy);
        assert_eq!(shared.decoder.get_complexity().unwrap(), 0);
        roster(&mut s, vec![member(10, 10)], at);
        let remote = s.remotes[0].as_mut().unwrap();
        assert!(remote.jitter.deep_redundancy);
        assert_eq!(remote.decoder.get_complexity().unwrap(), 5);
        assert_eq!(remote.jitter.metrics().target_ms, 80.0);
        let missing = media(&s, None, None, 31, at);
        s.receive_command(missing, false, at).unwrap();
        assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
        let valid = media(&s, Some(10), Some(10), 31, at);
        s.receive_command(valid, false, at).unwrap();
        let generation = generation(&s);
        let later = at + Duration::from_millis(20);
        roster(&mut s, vec![member(10, 10)], later);
        let valid = media(&s, Some(10), Some(10), 32, later);
        s.receive_command(valid, false, later).unwrap();
        assert_eq!(
            super::generation(&s),
            generation,
            "ordinary packets/rosters must not restart playout"
        );
        assert_eq!(s.remotes[0].as_ref().unwrap().since, at);
        s.set_deep_redundancy(false).unwrap();
        s.settings.deep_redundancy = false;
        let remote = s.remotes[0].as_mut().unwrap();
        assert!(!remote.jitter.deep_redundancy);
        assert_eq!(remote.decoder.get_complexity().unwrap(), 0);
        assert_eq!(remote.jitter.metrics().target_ms, 40.0);
        assert_eq!(remote.jitter.metrics().packets, 0);
    }
}

#[test]
fn dred_roster_restrictions_and_same_account_replacement_recreate_neural_state() {
    let (mut s, mut mixer) = configured(false);
    let at = Instant::now();
    roster(&mut s, vec![member(10, 10)], at);
    for (i, restriction) in ["muted", "deafened", "permission"].into_iter().enumerate() {
        dirty(&mut s);
        let privacy = 20 + i as u32 * 10;
        let mut restricted = member(10, privacy);
        restricted.muted = restriction == "muted";
        restricted.deafened = restriction == "deafened";
        restricted.can_speak = restriction != "permission";
        roster(&mut s, vec![restricted], at);
        assert!(s.remotes[0].is_none());
        assert_silent(&mut mixer);
        roster(&mut s, vec![member(10, privacy - 1)], at);
        assert!(s.remotes[0].is_none());
        roster(&mut s, vec![member(10, privacy + 1)], at);
        assert_fresh(s.remotes[0].as_mut().unwrap());
    }
    for source in [50, 60, 70] {
        dirty(&mut s);
        let queued = media(&s, Some(source - 10), Some(source - 10), 90, at);
        roster(&mut s, vec![member(source, source)], at);
        s.receive_command(queued, false, at).unwrap();
        // Even a forward privacy claim cannot authorize an old stable source.
        let delayed = media(&s, Some(source - 10), Some(1000), 91, at);
        s.receive_command(delayed, false, at).unwrap();
        roster(&mut s, vec![member(source - 10, 1000)], at);
        let remote = s.remotes[0].as_mut().unwrap();
        assert_eq!(remote.source_epoch, source);
        assert_eq!(remote.jitter.metrics().packets, 0);
        assert_eq!(remote.decode_stats.dred_attempts, 0);
        assert_fresh(remote);
        assert_silent(&mut mixer);
    }
}

#[test]
fn dred_forward_privacy_flushes_donors_pcm_neural_state_without_roster_delay() {
    let (mut s, mut mixer) = configured(false);
    let at = Instant::now();
    roster(&mut s, vec![member(10, 10)], at);
    dirty(&mut s);
    for index in [30, 33] {
        let old = media(&s, Some(10), Some(10), index, at);
        s.receive_command(old, false, at).unwrap();
    }
    let next = at + Duration::from_millis(20);
    let fresh = media(&s, Some(10), Some(11), 34, next);
    s.receive_command(fresh, false, next).unwrap();
    assert_silent(&mut mixer);
    let remote = s.remotes[0].as_mut().unwrap();
    assert_eq!(remote.jitter.metrics().packets, 1);
    assert_eq!(remote.privacy_epoch, 11);
    assert_fresh(remote);
    let stale = media(&s, Some(10), Some(10), 35, next);
    s.receive_command(stale, false, next).unwrap();
    roster(&mut s, vec![member(10, 10)], next);
    assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 1);
    assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 11);
}

#[test]
fn dred_receive_floor_stale_ack_and_effective_tombstone_reject_delayed_donors() {
    let (mut s, mut mixer) = configured(false);
    let at = Instant::now();
    roster(&mut s, vec![member(10, 10)], at);
    // Acknowledge initial discard before testing the independent urgent guard.
    assert_silent(&mut mixer);
    dirty(&mut s);
    assert!(s.devices.writers[0].ready());
    s.transmit_control.enable_receive_protection();
    assert!(
        s.transmit_control
            .admit_receive(s.transmit_control.generation())
    );
    assert!(s.transmit_control.receive_allowed());
    s.transmit_control.invalidate();
    // Callback guard acts before the worker processes the boundary command.
    assert_silent(&mut mixer);
    let previous = s.transmit_control.generation();
    boundary(&mut s, previous, None, at);
    assert_fresh(s.remotes[0].as_mut().unwrap());
    s.transmit_control.invalidate();
    let current = s.transmit_control.generation();
    boundary(&mut s, previous, Some(50), at);
    assert!(!s.transmit_control.receive_allowed());
    boundary(&mut s, current, Some(60), at);
    assert!(s.transmit_control.receive_allowed());
    let delayed = media(&s, Some(10), Some(59), 33, at);
    s.receive_command(delayed, false, at).unwrap();
    assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
    assert_eq!(s.remotes[0].as_ref().unwrap().decode_stats.dred_attempts, 1);
    let new = media(&s, Some(10), Some(100), 34, at);
    s.receive_command(new, false, at).unwrap();
    let mut restricted = member(10, 61);
    restricted.muted = true;
    roster(&mut s, vec![restricted], at);
    assert!(s.remotes[0].is_none());
    roster(&mut s, vec![member(10, 60)], at);
    assert!(s.remotes[0].is_none());
    roster(&mut s, vec![member(10, 62)], at);
    assert_eq!(s.remotes[0].as_ref().unwrap().privacy_epoch, 100);
    let delayed = media(&s, Some(10), Some(62), 35, at);
    s.receive_command(delayed, false, at).unwrap();
    assert_eq!(s.remotes[0].as_ref().unwrap().jitter.metrics().packets, 0);
    assert_fresh(s.remotes[0].as_mut().unwrap());
    assert_silent(&mut mixer);
}

#[test]
fn dred_real_padding_red_combination_recovers_burst_through_worker_ingress() {
    use thiscord_shared::rtp_red as wire;
    for adaptive_enabled in [false, true] {
        for dred_enabled in [false, true] {
            for red_enabled in [false, true] {
                let (mut s, mut mixer) = configured(red_enabled);
                s.set_deep_redundancy(dred_enabled).unwrap();
                s.settings.deep_redundancy = dred_enabled;
                s.settings.adaptive_fec = adaptive_enabled;
                let mut encoder = transmit::Encoder::new(true).unwrap();
                encoder.set_deep_redundancy(dred_enabled).unwrap();
                let speech = fixture::speech();
                let at = Instant::now();
                roster(&mut s, vec![member(10, 10)], at);
                let mut sender = red::Sender::default();
                let connection = s.connection.clone().unwrap();
                let mut padded = 0;
                let mut first_decode = None;
                for ms in 0..2000usize {
                    let now = at + Duration::from_millis(ms as u64);
                    if ms.is_multiple_of(20) {
                        let index = ms / 20;
                        encoder.adapt(adaptive_enabled, 5, None, now).unwrap();
                        let mut buffer = [0; 1500];
                        let (len, _, _) = encoder
                            .encode(&speech.as_chunks::<FRAME>().0[index], true, &mut buffer)
                            .unwrap();
                        let frame = outgoing::EncodedFrame {
                            accounted: true,
                            packet_redundancy: red_enabled,
                            payload: Bytes::copy_from_slice(&buffer[..len]),
                            media_position: (index * FRAME) as u64,
                            marker: false,
                            oldest_capture: now,
                            generation: 0,
                            connection: connection.clone(),
                            delivery: connection.clone(),
                            activation_delay: Duration::ZERO,
                        };
                        assert!(sender.within_budget(&frame));
                        let (pt, bytes) = sender.packet(&frame, red_enabled, now);
                        if (frame.payload[0] & 3) == 3 && (frame.payload[1] & 0x40) != 0 {
                            padded += 1;
                            if pt == wire::PAYLOAD_TYPE {
                                assert!(wire::parse(&bytes).unwrap().history.is_empty());
                                assert_eq!(bytes.len(), frame.payload.len() + 1);
                            } else {
                                assert_eq!(bytes, frame.payload);
                            }
                        }
                        assert!(bytes.len() <= wire::MAX_PAYLOAD);
                        sender.admitted(frame, red_enabled);
                        if !(40..=42).contains(&index) {
                            let mut command = media(&s, Some(10), Some(10), index, now);
                            if let Command::Packet {
                                payload_type,
                                payload,
                                ..
                            } = &mut command
                            {
                                *payload_type = pt;
                                *payload = bytes;
                            }
                            s.receive_command(command, false, now).unwrap();
                        }
                    }
                    if ms.is_multiple_of(5) {
                        let remote = s.remotes[0].as_mut().unwrap();
                        remote.refill(&mut s.devices.writers[0], now);
                        if remote.decode_stats.normal_frames > 0 {
                            first_decode.get_or_insert(ms);
                        }
                    }
                    mixer.render_at(&mut [0.0; 48], 1, now);
                }
                if dred_enabled {
                    assert!(padded > 50, "real DRED extension emission");
                }
                assert_eq!(first_decode, Some(if dred_enabled { 80 } else { 40 }));
                let stats = &s.remotes[0].as_ref().unwrap().decode_stats;
                if dred_enabled {
                    assert!(stats.dred_output_frames >= 2);
                    assert_eq!(stats.dred_attempts, 3);
                } else {
                    assert_eq!(stats.dred_attempts, 0);
                }
                eprintln!(
                    "worker ingress FEC={adaptive_enabled} RED={red_enabled} DRED={dred_enabled}: startup={first_decode:?}ms DRED={} PLC={} padded={padded}",
                    stats.dred_output_frames, stats.plc_frames
                );
            }
        }
    }
}
