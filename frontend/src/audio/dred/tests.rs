use super::*;
use crate::audio::{
    codec, fixture,
    jitter::{Jitter, Playout},
    mixer::RATE,
    transmit,
};
use std::time::{Duration, Instant};

fn decoder(enabled: bool) -> opus::Decoder {
    let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    decoder.set_complexity(if enabled { 5 } else { 0 }).unwrap();
    decoder
}
fn packets(enabled: bool) -> Vec<Bytes> {
    let mut encoder = transmit::Encoder::new(true).unwrap();
    encoder.set_deep_redundancy(enabled).unwrap();
    fixture::speech()
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|pcm| {
            let mut output = [0; 1500];
            let (n, omitted, _) = encoder.encode(pcm, true, &mut output).unwrap();
            assert!(!omitted);
            Bytes::copy_from_slice(&output[..n])
        })
        .collect()
}

#[test]
fn real_dred_recovers_isolated_and_three_packet_bursts_before_deadline() {
    let started = Instant::now();
    let encoded = packets(true);
    let baseline_packets = packets(false);
    for burst in [1, 2, 3] {
        let mut jitter = Jitter::default();
        jitter.deep_redundancy = true;
        let mut dec = decoder(true);
        // Same packets and same neural PLC, but without redundancy: distinguishes
        // real DRED features from merely enabling a better PLC algorithm.
        let mut plc = decoder(true);
        let mut stats = codec::DecodeStats::default();
        let mut distinct = 0;
        let mut beyond_adjacent = 0;
        let origin = Instant::now();
        let start_ts = u32::MAX - 17 * FRAME as u32;
        let start_seq = u16::MAX - 17;
        let missing = |i: usize| i >= 20 && (i % 23) < burst;
        for i in 0..encoded.len() - 4 {
            // Initial lookahead, then one arrival every 20 ms, always before its
            // deadline. No decoding after a deadline or synthetic history drain.
            let arrivals = if i == 0 { 0..4 } else { i + 3..i + 4 };
            for n in arrivals {
                if !missing(n) {
                    let at = origin + Duration::from_millis(n as u64 * 20);
                    jitter.push(
                        start_seq.wrapping_add(n as u16),
                        start_ts.wrapping_add(n as u32 * FRAME as u32),
                        encoded[n].clone(),
                        at,
                        at,
                    );
                }
            }
            let decision = jitter
                .pop(true, origin + Duration::from_millis((i as u64 + 4) * 20))
                .expect("one media slot");
            let mut output = [0.0; FRAME];
            let recovered_before = stats.dred_output_frames;
            codec::decode(&mut dec, &decision, &mut output, &mut stats);
            let mut no_redundancy = [0.0; FRAME];
            plc.decode_float(
                if missing(i) { &[] } else { &encoded[i] },
                &mut no_redundancy,
                false,
            )
            .unwrap();
            if missing(i) {
                assert!(
                    matches!(decision, Playout::Dred(..)),
                    "missing {i}: {decision:?}"
                );
                if stats.dred_output_frames > recovered_before {
                    if output
                        .iter()
                        .zip(no_redundancy)
                        .any(|(a, b)| (a - b).abs() > 1e-5)
                    {
                        distinct += 1;
                    }
                    if matches!(decision, Playout::Dred(_, offset) if offset > FRAME as u32) {
                        beyond_adjacent += 1;
                    }
                }
                // The late original cannot be replayed after recovered playout.
                let at = origin + Duration::from_millis((i as u64 + 4) * 20);
                assert!(!jitter.push(
                    start_seq.wrapping_add(i as u16),
                    start_ts.wrapping_add(i as u32 * FRAME as u32),
                    encoded[i].clone(),
                    at,
                    at
                ));
            }
            assert!(output.iter().all(|x| x.is_finite()));
        }
        assert!(
            stats.dred_output_frames > 5,
            "DRED was not present: {}",
            stats.dred_output_frames
        );
        assert!(stats.dred_output_frames * 2 >= stats.dred_attempts);
        assert!(distinct > 5);
        if burst > 1 {
            assert!(beyond_adjacent > 5);
        }
        assert_eq!(stats.decode_errors, 0);
        eprintln!(
            "burst={burst}: DRED={}/{}, distinct from neural PLC={distinct}, beyond predecessor={beyond_adjacent}",
            stats.dred_output_frames, stats.dred_attempts
        );
    }
    let bytes: usize = encoded.iter().map(Bytes::len).sum();
    let padded = encoded
        .iter()
        .filter(|p| p.len() >= 2 && p[0] & 3 == 3 && p[1] & 0x40 != 0)
        .count();
    assert!(
        padded > encoded.len() / 2,
        "RED must recognize DRED padding"
    );
    assert!(encoded.iter().all(|p| p.len() <= 1100));
    let base: usize = baseline_packets.iter().map(Bytes::len).sum();
    let seconds = encoded.len() as f64 * 0.02;
    eprintln!(
        "fixture {seconds:.2}s: DRED {bytes} bytes ({:.1} kbit/s), baseline {base} ({:.1} kbit/s), payload ratio {:.3}; test elapsed {:?}; target 80ms, history 60ms",
        bytes as f64 * 8.0 / seconds / 1000.0,
        base as f64 * 8.0 / seconds / 1000.0,
        bytes as f64 / base as f64,
        started.elapsed()
    );
}

#[test]
fn absent_dred_malformed_and_out_of_range_history_fall_back_safely() {
    let plain = packets(false);
    let mut dec = decoder(true);
    let mut pcm = [0.0; FRAME];
    let mut stats = codec::DecodeStats::default();
    codec::decode(
        &mut dec,
        &Playout::Dred(plain[30].clone(), 1920),
        &mut pcm,
        &mut stats,
    );
    assert_eq!(stats.dred_output_frames, 0);
    assert_eq!(stats.plc_frames, 1);
    codec::decode(
        &mut dec,
        &Playout::Dred(plain[31].clone(), 960),
        &mut pcm,
        &mut stats,
    );
    assert_eq!(stats.fec_attempts, 1);
    for bad in [
        Bytes::new(),
        Bytes::from_static(&[3, 0x82]),
        Bytes::from(vec![0; 1501]),
    ] {
        codec::decode(&mut dec, &Playout::Dred(bad, 1920), &mut pcm, &mut stats);
        assert!(pcm.iter().all(|x| x.is_finite()));
    }
    assert_eq!(stats.dred_output_frames, 0);
    let encoded = packets(true);
    if let Some(data) = opus::Dred::parse(&encoded[30], HISTORY as i32, RATE).unwrap() {
        for offset in [-1, 0, 959, 2881, i32::MAX] {
            assert_eq!(data.decode_float(&mut dec, offset, &mut pcm).unwrap(), None);
        }
        assert!(data.decode_float(&mut dec, 960, &mut []).is_err());
    } else {
        panic!("fixture must contain DRED");
    }
}

#[test]
fn donor_rejects_dtx_capture_gaps_boundaries_and_keeps_wrap_order() {
    let at = Instant::now();
    let cursor = u32::MAX - 479;
    let mut packet = Packet {
        sequence: 1,
        timestamp: cursor.wrapping_add(1920),
        marker: false,
        no_data: false,
        arrived: at,
        payload: Bytes::from_static(b"fixture"),
    };
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_some());
    packet.marker = true;
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    packet.marker = false;
    packet.no_data = true;
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    packet.no_data = false;
    packet.sequence = 65535; // timestamp gap with no transmitted packet loss
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    packet.sequence = 1;
    packet.timestamp = cursor.wrapping_add(1921);
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    packet.timestamp = cursor.wrapping_add(3840);
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    packet.timestamp = cursor.wrapping_sub(960);
    assert!(donor(std::slice::from_ref(&packet), cursor, Some(65534), 0).is_none());
    assert!(donor(&[], cursor, None, 0).is_none());
}

#[test]
fn encoder_and_neural_decoder_resets_remove_all_previous_speech() {
    let encoded = packets(true);
    let mut dec = decoder(true);
    let mut output = [0.0; FRAME];
    let mut stats = codec::DecodeStats::default();
    for p in &encoded[..30] {
        dec.decode_float(p, &mut output, false).unwrap();
    }
    assert!(decode(
        &mut dec,
        &encoded[32],
        1920,
        &mut output,
        &mut stats
    ));
    assert!(codec::reset(&mut dec, &mut stats));
    let mut fresh = decoder(true);
    let mut clean = [0.0; FRAME];
    dec.decode_float(&[], &mut output, false).unwrap();
    fresh.decode_float(&[], &mut clean, false).unwrap();
    assert!(
        output == clean,
        "PLC immediately after reset must match fresh state"
    );
    for p in &packets(false)[..5] {
        dec.decode_float(p, &mut output, false).unwrap();
        fresh.decode_float(p, &mut clean, false).unwrap();
        assert!(output == clean, "reset state must match fresh decoder");
    }
    let mut encoder = transmit::Encoder::new(true).unwrap();
    encoder.set_deep_redundancy(true).unwrap();
    let mut bytes = [0; 1500];
    for pcm in fixture::speech().as_chunks::<FRAME>().0.iter().take(30) {
        encoder.encode(pcm, true, &mut bytes).unwrap();
    }
    encoder.reset().unwrap();
    let mut fresh = transmit::Encoder::new(true).unwrap();
    fresh.set_deep_redundancy(true).unwrap();
    let mut other = [0; 1500];
    for pcm in fixture::speech().as_chunks::<FRAME>().0.iter().take(10) {
        let (n, _, _) = encoder.encode(pcm, true, &mut bytes).unwrap();
        let (m, _, _) = fresh.encode(pcm, true, &mut other).unwrap();
        assert_eq!(&bytes[..n], &other[..m]);
    }
}

fn remote(at: Instant) -> crate::audio::Remote {
    crate::audio::Remote {
        source_epoch: 1,
        privacy_epoch: 1,
        since: at,
        deafened: false,
        decode_stats: Default::default(),
        activity: Default::default(),
        id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        label: "DRED fixture".into(),
        jitter: Default::default(),
        decoder: decoder(false),
        rate: Default::default(),
    }
}

#[test]
fn production_playout_obeys_deadline_and_recovers_three_packet_burst() {
    use crate::audio::mixer;
    use std::sync::{Arc, atomic::Ordering};
    let encoded = packets(true);
    for late in [false, true] {
        let at = Instant::now() + Duration::from_secs(1);
        let mut remote = remote(at);
        let controls = Arc::new(mixer::Controls::default());
        let (mut writers, mut mixer) = mixer::mixer(controls);
        let writer = &mut writers[0];
        writer.control.active.store(true, Ordering::Release);
        remote.set_deep_redundancy(writer, true, at).unwrap();
        let mut first_decode = None;
        let mut first_recovery = None;
        let mut recovered_slots = Vec::new();
        for ms in 0..2000usize {
            let now = at + Duration::from_millis(ms as u64);
            if ms.is_multiple_of(20) {
                let n = ms / 20;
                if !(40..=42).contains(&n) && (!late || !(43..=47).contains(&n)) {
                    remote.push(n as u16, (n * FRAME) as u32, encoded[n].clone(), now, now);
                }
                if late && ms >= 160 {
                    let delayed = (ms - 160) / 20;
                    if (43..=47).contains(&delayed) {
                        assert!(
                            !remote.push(
                                delayed as u16,
                                (delayed * FRAME) as u32,
                                encoded[delayed].clone(),
                                now,
                                now
                            ),
                            "late donor must not replay expired media"
                        );
                    }
                }
            }
            if ms.is_multiple_of(5) {
                let previous = remote.decode_stats.dred_output_frames;
                remote.refill(writer, now);
                if remote.decode_stats.dred_output_frames > previous {
                    // With 1ms callbacks this run refills one recovered slot at
                    // a time. Verify its own deadline, not the end of the burst.
                    assert_eq!(remote.decode_stats.dred_output_frames - previous, 1);
                    let decoded = remote.decode_stats.normal_frames
                        + remote.decode_stats.dred_output_frames
                        + remote.decode_stats.fec_output_frames
                        + remote.decode_stats.plc_frames;
                    let slot = decoded as usize - 1;
                    assert!((40..=42).contains(&slot));
                    assert!(
                        ms <= 80 + slot * 20,
                        "slot {slot} decoded after its deadline at {ms}ms"
                    );
                    recovered_slots.push(slot);
                }
                if remote.decode_stats.normal_frames > 0 {
                    first_decode.get_or_insert(ms);
                }
                if remote.decode_stats.dred_output_frames > 0 {
                    first_recovery.get_or_insert(ms);
                }
            }
            let mut pcm = [0.0_f32; 48];
            mixer.render(&mut pcm, 1);
            assert!(pcm.iter().all(|x| x.is_finite()));
            assert!(remote.jitter.metrics().packets <= 12);
        }
        assert_eq!(first_decode, Some(80), "enabled startup buffer");
        if late {
            assert_eq!(remote.decode_stats.dred_output_frames, 0);
            assert!(remote.decode_stats.plc_frames >= 3);
        } else {
            assert!(remote.decode_stats.dred_output_frames >= 2);
            assert_eq!(remote.decode_stats.dred_attempts, 3);
            assert_eq!(recovered_slots, [41, 42]);
            assert!(first_recovery.is_some_and(|ms| ms <= 900));
        }
        eprintln!(
            "production late={late}: first decode={first_decode:?}ms; first DRED={first_recovery:?}ms, outputs={}, PLC={}",
            remote.decode_stats.dred_output_frames, remote.decode_stats.plc_frames
        );
    }
}

#[test]
fn deafen_settings_and_source_recreation_discard_queued_dred() {
    use crate::audio::mixer;
    use std::sync::{Arc, atomic::Ordering};
    let at = Instant::now() + Duration::from_secs(1);
    let encoded = packets(true);
    let (mut writers, mut mixer) = mixer::mixer(Arc::new(mixer::Controls::default()));
    let writer = &mut writers[0];
    writer.control.active.store(true, Ordering::Release);
    let mut speaker = remote(at);
    speaker.set_deep_redundancy(writer, true, at).unwrap();
    assert_eq!(speaker.jitter.metrics().target_ms, 80.0);
    for n in [20, 23, 24] {
        speaker.push(n as u16, (n * FRAME) as u32, encoded[n].clone(), at, at);
    }
    speaker.refill(writer, at + Duration::from_millis(80));
    speaker.set_deafened(writer, true, at + Duration::from_millis(81));
    let mut pcm = [1.0_f32; FRAME];
    mixer.render(&mut pcm, 1);
    assert!(pcm.iter().all(|x| *x == 0.0));
    assert_eq!(speaker.jitter.metrics().packets, 0);
    speaker.set_deafened(writer, false, at + Duration::from_millis(90));
    assert!(!speaker.push(
        23,
        23 * FRAME as u32,
        encoded[23].clone(),
        at,
        at + Duration::from_millis(90)
    ));
    speaker
        .set_deep_redundancy(writer, false, at + Duration::from_millis(91))
        .unwrap();
    assert_eq!(speaker.jitter.metrics().target_ms, 40.0);
    // Replacement is a new decoder and receipt fence; the RED dependency adds
    // the roster epoch that invokes this path for the same account as well.
    let replacement_at = at + Duration::from_secs(1);
    let mut replacement = remote(replacement_at);
    assert!(!replacement.push(
        23,
        23 * FRAME as u32,
        encoded[23].clone(),
        at,
        replacement_at
    ));
    assert_eq!(replacement.decode_stats.dred_output_frames, 0);
}

#[test]
fn codec_worker_cost_is_measured_without_device_or_network_claims() {
    let speech = fixture::speech();
    for enabled in [false, true] {
        let mut encoder = transmit::Encoder::new(true).unwrap();
        encoder.set_deep_redundancy(enabled).unwrap();
        let mut dec = decoder(enabled);
        let mut stats = codec::DecodeStats::default();
        let mut decode_time = Duration::ZERO;
        let mut decode_max = Duration::ZERO;
        for (i, pcm) in speech.as_chunks::<FRAME>().0.iter().enumerate() {
            let mut bytes = [0; 1100];
            let (n, _, _) = encoder.encode(pcm, true, &mut bytes).unwrap();
            let mut output = [0.0; FRAME];
            let timer = Instant::now();
            // Independent bounded 60ms parse per decision, with 20ms synthesis.
            if enabled && i > 20 {
                decode(&mut dec, &bytes[..n], 960, &mut output, &mut stats);
            } else {
                dec.decode_float(&bytes[..n], &mut output, false).unwrap();
            }
            let elapsed = timer.elapsed();
            decode_time += elapsed;
            decode_max = decode_max.max(elapsed);
        }
        eprintln!(
            "worker enabled={enabled}: {}x20ms frames; encode {:.3}ms total, decode {:?} total / {:?} max; DRED output={}; not a CPU deadline assertion",
            encoder.stats.encoded_frames,
            encoder.stats.encode_ns as f64 / 1e6,
            decode_time,
            decode_max,
            stats.dred_output_frames
        );
    }
}

#[test]
fn settings_defaults_roundtrip_and_dtx_omissions_survive() {
    let old: thiscord_shared::audio::AudioSettings = serde_json::from_str("{}").unwrap();
    assert!(!old.deep_redundancy);
    let settings = thiscord_shared::audio::AudioSettings {
        deep_redundancy: true,
        ..old
    };
    assert_eq!(
        settings,
        serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap()
    );
    let mut encoder = transmit::Encoder::new(true).unwrap();
    encoder.set_deep_redundancy(true).unwrap();
    let mut output = [0; 1500];
    for _ in 0..300 {
        encoder.encode(&[0.0; FRAME], false, &mut output).unwrap();
    }
    assert!(encoder.stats.omitted_packets > 250);
    encoder.set_deep_redundancy(false).unwrap();
    let mut fresh = transmit::Encoder::new(true).unwrap();
    let pcm = [0.1; FRAME];
    let (n, _, _) = encoder.encode(&pcm, true, &mut output).unwrap();
    let mut other = [0; 1500];
    let (m, _, _) = fresh.encode(&pcm, true, &mut other).unwrap();
    assert_eq!(&output[..n], &other[..m]);
}
