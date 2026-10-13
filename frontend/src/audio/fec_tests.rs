use super::*;
use crate::audio::{
    Remote, fixture,
    jitter::Jitter,
    mixer::{self, Controls},
};
use bytes::Bytes;
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

fn decoder() -> opus::Decoder {
    opus::Decoder::new(RATE, opus::Channels::Mono).unwrap()
}
fn packets(fec: bool) -> (Vec<f32>, Vec<Bytes>, usize) {
    let speech = fixture::speech();
    let mut encoder = microphone_encoder().unwrap();
    assert!(encoder.get_inband_fec().unwrap());
    assert_eq!(encoder.get_packet_loss_perc().unwrap(), 5);
    assert_eq!(encoder.get_bitrate().unwrap(), opus::Bitrate::Bits(32_000));
    encoder.set_inband_fec(fec).unwrap();
    let delay = encoder.get_lookahead().unwrap() as usize;
    let packets = speech
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|pcm| {
            let mut encoded = [0; 1500];
            let n = encoder.encode_float(pcm, &mut encoded).unwrap();
            Bytes::copy_from_slice(&encoded[..n])
        })
        .collect();
    (speech, packets, delay)
}
fn decoded(decoder: &mut opus::Decoder, packet: &[u8], fec: bool) -> [f32; FRAME] {
    let mut pcm = [0.0; FRAME];
    assert_eq!(decoder.decode_float(packet, &mut pcm, fec).unwrap(), FRAME);
    pcm
}
fn remote(now: Instant) -> Remote {
    Remote {
        since: now,
        deafened: false,
        decode_stats: Default::default(),
        activity: Default::default(),
        id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        label: "fixture".into(),
        rate: Default::default(),
        jitter: Jitter::default(),
        decoder: decoder(),
    }
}

#[test]
fn speech_fec_differs_from_plc_and_preserves_successor_continuity() {
    let (speech, packets, delay) = packets(true);
    let (mut fec, mut plc, mut reference) = (decoder(), decoder(), decoder());
    let mut stats = DecodeStats::default();
    let (mut fec_error, mut plc_error, mut reference_error) = (0.0, 0.0, 0.0);
    let (mut attempts, mut distinct) = (0, 0);
    for i in 0..packets.len() - 1 {
        let missing = i > 10 && i % 11 == 0;
        let normal = decoded(&mut reference, &packets[i], false);
        let decision = if missing {
            Playout::Fec(packets[i + 1].clone())
        } else {
            Playout::Packet(packets[i].clone())
        };
        let mut output = [0.0; FRAME];
        assert_eq!(
            decode(&mut fec, &decision, &mut output, &mut stats),
            !missing
        );
        let baseline = decoded(&mut plc, if missing { &[] } else { &packets[i] }, false);
        if missing {
            attempts += 1;
            if output
                .iter()
                .zip(baseline)
                .any(|(a, b)| (a - b).abs() > 1e-5)
            {
                distinct += 1;
            }
            // Both outputs have the same codec delay; compare aligned speech,
            // not an equality assertion against lossy source PCM.
            let clean = &speech[i * FRAME - delay..(i + 1) * FRAME - delay];
            for ((&x, &y), (&z, &r)) in clean.iter().zip(&output).zip(baseline.iter().zip(&normal))
            {
                fec_error += f64::from(x - y).powi(2);
                plc_error += f64::from(x - z).powi(2);
                reference_error += f64::from(x - r).powi(2);
            }
        }
        assert!(output.iter().all(|s| s.is_finite()));
    }
    assert!(
        attempts > 5 && distinct > 5,
        "fixture must exercise actual redundancy, not just PLC"
    );
    assert_eq!(stats.fec_attempts, attempts);
    assert_eq!(stats.fec_output_frames, attempts);
    assert_eq!(
        stats.normal_frames + stats.fec_output_frames,
        (packets.len() - 1) as u64
    );
    assert_eq!(stats.decode_errors, 0);
    eprintln!(
        "aligned lost-frame squared error: FEC={fec_error:.4}, PLC={plc_error:.4}, lossless-transport Opus={reference_error:.4}; distinct FEC intervals={distinct}/{attempts}"
    );
    assert!(
        fec_error < plc_error,
        "fixed synthetic speech regression, not an acoustic quality claim"
    );
}

#[test]
fn absent_redundancy_is_counted_as_an_attempt_not_verified_recovery() {
    let (_, packets, _) = packets(false);
    let (mut fec, mut plc) = (decoder(), decoder());
    for packet in &packets[..20] {
        assert!(decoded(&mut fec, packet, false) == decoded(&mut plc, packet, false));
    }
    let mut stats = DecodeStats::default();
    let mut pcm = [0.0; FRAME];
    assert!(!decode(
        &mut fec,
        &Playout::Fec(packets[21].clone()),
        &mut pcm,
        &mut stats
    ));
    assert!(pcm == decoded(&mut plc, &[], false));
    assert!(decoded(&mut fec, &packets[21], false) == decoded(&mut plc, &packets[21], false));
    assert_eq!(stats.fec_attempts, 1);
    assert_eq!(stats.fec_output_frames, 1); // Includes implicit PLC.
    assert_eq!(stats.plc_frames, 0); // Only explicit PLC calls are counted here.
}

#[test]
fn malformed_normal_and_fec_packets_fall_back_without_losing_duration() {
    let (_, packets, _) = packets(true);
    let mut dec = decoder();
    let mut stats = DecodeStats::default();
    let mut pcm = [f32::NAN; FRAME];
    // A valid duration header with invalid VBR framing reaches the actual Opus
    // error path, independently of our empty/duration validation checks.
    assert_eq!(dec.get_nb_samples(&[3, 0x82]).unwrap(), FRAME);
    assert!(dec.decode_float(&[3, 0x82], &mut pcm, false).is_err());
    for bad in [Bytes::new(), Bytes::from_static(&[3, 0x82])] {
        for decision in [Playout::Packet(bad.clone()), Playout::Fec(bad.clone())] {
            assert!(!decode(&mut dec, &decision, &mut pcm, &mut stats));
            assert!(pcm.iter().all(|s| s.is_finite()));
        }
    }
    assert_eq!(stats.decode_errors, 4);
    assert_eq!(stats.plc_frames, 4);
    let mut encoder = microphone_encoder().unwrap();
    for duration in [FRAME / 2, FRAME * 2] {
        let mut bytes = [0; 1500];
        let n = encoder
            .encode_float(&vec![0.01; duration], &mut bytes)
            .unwrap();
        assert!(!decode(
            &mut dec,
            &Playout::Packet(Bytes::copy_from_slice(&bytes[..n])),
            &mut pcm,
            &mut stats
        ));
        assert!(pcm.iter().all(|s| s.is_finite()));
    }
    assert_eq!(stats.decode_errors, 6);
    assert_eq!(stats.plc_frames, 6);
    assert!(decode(
        &mut dec,
        &Playout::Packet(packets[0].clone()),
        &mut pcm,
        &mut stats
    ));
    assert_eq!(stats.normal_frames, 1);
}

#[test]
fn deterministic_transport_loss_matches_ordered_decoder_and_has_no_playout_gaps() {
    let (_, packets, _) = packets(true);
    let count = 100.min(packets.len() - 4);
    let drops = [15, 30, 31, 47];
    let mut expected_decoder = decoder();
    let mut expected = Vec::new();
    for i in 0..count + 4 {
        let pcm = if drops.contains(&i) {
            if drops.contains(&(i + 1)) {
                decoded(&mut expected_decoder, &[], false)
            } else {
                decoded(&mut expected_decoder, &packets[i + 1], true)
            }
        } else {
            decoded(&mut expected_decoder, &packets[i], false)
        };
        expected.extend_from_slice(&pcm);
    }
    let start = Instant::now();
    let mut remote = remote(start);
    let controls = Arc::new(Controls::default());
    let (mut writers, mut mixer) = mixer::mixer(controls.clone());
    let writer = &mut writers[0];
    writer.control.active.store(true, Ordering::Release);
    let mut actual = Vec::new();
    let mut seed = 0x12345678_u32;
    let arrivals: Vec<_> = (0..count + 4)
        .map(|i| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            // Some originals reorder behind successors, but arrive before deadline.
            i * 20
                + if i > 2 && i % 13 == 0 {
                    28
                } else {
                    (seed as usize) % 8
                }
        })
        .collect();
    let mut next_wake = 60;
    let mut rate = crate::audio::playout_rate::Rate::default();
    let mut rendered_reference = Vec::new();
    let mut reference_frames = 0;
    for ms in 0..60 + count * 20 {
        let now = start + Duration::from_millis(ms as u64);
        for (i, &arrival) in arrivals.iter().enumerate() {
            if !drops.contains(&i) && (ms == arrival || ms == arrival + 3) {
                remote.jitter.push(
                    (i as u16).wrapping_add(65530),
                    (i as u32 * FRAME as u32).wrapping_add(u32::MAX - 479),
                    packets[i].clone(),
                    now,
                    now,
                );
            }
        }
        if ms >= next_wake {
            remote.refill(writer, now);
            // The expected codec order is independent of the scheduler. Apply
            // only its measured rate to compare the actual rendered samples.
            let frames = remote.decode_stats.normal_frames
                + remote.decode_stats.fec_output_frames
                + remote.decode_stats.plc_frames;
            while reference_frames < frames as usize && reference_frames < count + 4 {
                let mut output = [0.0; crate::audio::playout_rate::MAX_OUTPUT];
                let n = rate.process(
                    &expected[reference_frames * FRAME..(reference_frames + 1) * FRAME],
                    remote.jitter.metrics().correction_ppm,
                    &mut output,
                );
                rendered_reference.extend_from_slice(&output[..n]);
                reference_frames += 1;
            }
            next_wake += [5, 7, 4][ms % 3];
        }
        if ms >= 60 {
            let mut pcm = [0.0; 48];
            mixer.render(&mut pcm, 1);
            actual.extend_from_slice(&pcm);
        }
    }
    assert!(rendered_reference.len() >= actual.len());
    assert!(
        actual.iter().zip(&rendered_reference).all(|(a, b)| a == b),
        "ordered output differs first at sample {:?}",
        actual
            .iter()
            .zip(&rendered_reference)
            .position(|(a, b)| a != b)
    );
    assert_eq!(controls.underruns.load(Ordering::Relaxed), 0);
    assert_eq!(remote.decode_stats.fec_attempts, 3);
    assert_eq!(remote.decode_stats.plc_frames, 1);
    assert_eq!(remote.jitter.metrics().rebufferings, 0);
}

#[test]
fn recovered_pcm_obeys_deafen_and_generation_invalidation_and_isolates_speakers() {
    // Use 1 ms callbacks so each refill below isolates one decode decision.
    // Larger callbacks intentionally prefetch the normal FEC carrier as well.
    let render = |mixer: &mut mixer::Mixer, pcm: &mut [f32; FRAME]| {
        for block in pcm.chunks_mut(48) {
            mixer.render(block, 1);
        }
    };
    let (_, packets, _) = packets(true);
    let now = Instant::now();
    let controls = Arc::new(Controls::default());
    let (mut writers, mut mixer) = mixer::mixer(controls.clone());
    let mut speaker = remote(now);
    // Warm the real decoder on speech, then lose one frame.
    for packet in &packets[..20] {
        decoded(&mut speaker.decoder, packet, false);
    }
    for i in [20, 22, 23] {
        speaker.jitter.push(
            i as u16,
            i as u32 * FRAME as u32,
            packets[i].clone(),
            now,
            now,
        );
    }
    writers[0].control.active.store(true, Ordering::Release);
    let play_at = now + Duration::from_millis(40);
    speaker.refill(&mut writers[0], play_at);
    let mut pcm = [0.0; FRAME];
    render(&mut mixer, &mut pcm);
    speaker.activity = Default::default();
    speaker.refill(&mut writers[0], play_at + Duration::from_millis(20));
    assert_eq!(speaker.decode_stats.fec_attempts, 1);
    // The normal successor, not speculative FEC, can light the indicator.
    assert_eq!(speaker.decode_stats.normal_frames, 1);
    render(&mut mixer, &mut pcm);
    assert!(
        !speaker
            .activity
            .speaking(play_at + Duration::from_millis(20))
    );
    speaker.refill(&mut writers[0], play_at + Duration::from_millis(40));
    assert_eq!(speaker.decode_stats.normal_frames, 2);
    controls.deafen.store(true, Ordering::Release);
    render(&mut mixer, &mut pcm);
    assert!(pcm.iter().all(|&s| s == 0.0));
    controls.deafen.store(false, Ordering::Release);
    speaker.refill(&mut writers[0], now);
    // Replacing a source invalidates queued FEC and normal PCM together.
    writers[0].control.generation.fetch_add(1, Ordering::AcqRel);
    speaker = remote(now);
    render(&mut mixer, &mut pcm);
    assert!(pcm.iter().all(|&s| s == 0.0));
    assert!(speaker.jitter.pop(true, now).is_none());
    assert!(!speaker.activity.speaking(now));
    let mut independent = decoder();
    assert!(
        decoded(&mut speaker.decoder, &packets[0], false)
            == decoded(&mut independent, &packets[0], false)
    );
    // Stopping/deactivating a stream silences any newly queued recovered PCM.
    writers[0].write(&[0.1; FRAME]);
    writers[0].control.active.store(false, Ordering::Release);
    render(&mut mixer, &mut pcm);
    assert!(pcm.iter().all(|&s| s == 0.0));
}
