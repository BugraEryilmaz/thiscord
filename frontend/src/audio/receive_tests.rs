//! Simulated RTP/device events use the production jitter, Opus decoder and mixer.
use super::*;

fn remote(at: Instant) -> Remote {
    Remote {
        decode_stats: Default::default(),
        since: at,
        deafened: false,
        activity: Default::default(),
        id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        label: "synthetic".into(),
        rate: Default::default(),
        jitter: Default::default(),
        decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
    }
}
fn packets() -> Vec<bytes::Bytes> {
    let mut encoder =
        opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
    (0..6)
        .map(|index| {
            let pcm: [f32; FRAME] = std::array::from_fn(|i| {
                ((index * FRAME + i) as f32 * std::f32::consts::TAU * 440.0 / RATE as f32).sin()
                    * 0.1
            });
            let mut bytes = [0; 1500];
            let len = encoder.encode_float(&pcm, &mut bytes).unwrap();
            bytes::Bytes::copy_from_slice(&bytes[..len])
        })
        .collect()
}
fn decoded(decoder: &mut opus::Decoder, payload: &[u8]) -> [f32; FRAME] {
    let mut pcm = [0.0; FRAME];
    assert_eq!(
        decoder.decode_float(payload, &mut pcm, false).unwrap(),
        FRAME
    );
    pcm
}

#[test]
fn sender_drop_and_partial_gap_reach_real_playout_without_compressing_samples() {
    let at = Instant::now();
    let packets = packets();
    let mut remote = remote(at);
    // One unsent 20 ms frame, then a 137-sample capture gap. Sequences stay contiguous.
    let timestamps = [0, 1920, 3017, 3977];
    let mut rtp = outgoing::RtpClock::default();
    let events: Vec<_> = timestamps
        .into_iter()
        .map(|timestamp| {
            (
                frames::sample_duration(timestamp as usize),
                rtp.next(timestamp),
            )
        })
        .collect();
    let mut reference = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    let mut expected = decoded(&mut reference, &packets[0]).to_vec();
    expected.extend(decoded(&mut reference, &[]));
    expected.extend(decoded(&mut reference, &packets[1]));
    expected.extend([0.0; 137]);
    expected.extend(decoded(&mut reference, &packets[2]));
    let mut rate = playout_rate::Rate::default();
    let expected: Vec<_> = expected
        .chunks(FRAME)
        .flat_map(|pcm| {
            let mut output = [0.0; playout_rate::MAX_OUTPUT];
            let n = rate.process(pcm, 0.0, &mut output);
            output[..n].to_vec()
        })
        .collect();
    let controls = Arc::new(Controls::default());
    let (mut writers, mut mixer) = mixer(controls.clone());
    writers[0].control.active.store(true, Ordering::Release);
    let mut actual = Vec::new();
    let mut event = 0;
    for ms in 0..40 + expected.len().div_ceil(48) {
        let now = at + Duration::from_millis(ms as u64);
        while event < events.len() && at + events[event].0 <= now {
            let (arrival, (seq, ts)) = events[event];
            remote.push(seq, ts, packets[event].clone(), at + arrival, now);
            event += 1;
        }
        if ms % 5 == 0 {
            remote.refill(&mut writers[0], now);
        }
        if ms < 40 {
            mixer.render(&mut [0.0; 48], 1);
            continue;
        }
        if ms == 40 {
            controls.underruns.store(0, Ordering::Relaxed);
        }
        let mut output = [0.0; 48];
        let len = (expected.len() - actual.len()).min(48);
        mixer.render(&mut output[..len], 1);
        actual.extend_from_slice(&output[..len]);
    }
    assert!(
        actual == expected,
        "timestamp holes must survive continuous rate filtering"
    );
    assert_eq!(controls.underruns.load(Ordering::Relaxed), 0);
    assert_eq!(remote.jitter.diagnostics()["concealed_samples"], 1097);
}

#[test]
fn stalled_output_flushes_old_pcm_and_decoder_before_admitting_fresh_audio() {
    let at = Instant::now();
    let packets = packets();
    let mut remote = remote(at);
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    writers[0].control.active.store(true, Ordering::Release);
    for (i, payload) in packets.iter().take(3).enumerate() {
        remote.push(i as u16, i as u32 * 960, payload.clone(), at, at);
    }
    remote.refill(&mut writers[0], at + Duration::from_millis(40));
    assert!(remote.activity.speaking(at + Duration::from_millis(40)));
    // Leave decoded speech in the ring while both worker and output are stalled.
    let now = at + Duration::from_secs(2);
    for (i, payload) in packets.iter().enumerate().skip(3) {
        remote.push(
            i as u16,
            96000 + (i as u32 - 3) * 960,
            payload.clone(),
            now,
            now,
        );
    }
    remote.refill(&mut writers[0], now);
    assert!(!writers[0].ready());
    assert!(!remote.activity.speaking(now));
    // Repeated worker polls cannot decode ahead of the callback's acknowledgement.
    for _ in 0..10 {
        remote.refill(&mut writers[0], now);
    }
    let mut discarded = [1.0; 48];
    mixer.render(&mut discarded, 1);
    assert_eq!(discarded, [0.0; 48]);
    assert!(writers[0].ready());
    remote.refill(
        &mut writers[0],
        now + Duration::from_secs_f64(remote.jitter.metrics().target_ms / 1000.0),
    );
    let mut actual = [0.0; FRAME];
    mixer.render(&mut actual, 1);
    let mut clean = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    let mut rate = playout_rate::Rate::default();
    let mut expected = [0.0; playout_rate::MAX_OUTPUT];
    let n = rate.process(&decoded(&mut clean, &packets[3]), 0.0, &mut expected);
    assert!(actual == expected[..n]);
    assert_eq!(remote.jitter.diagnostics()["resets"], 1);
}

#[test]
fn gap_discovered_after_decode_still_requires_callback_flush_acknowledgement() {
    let at = Instant::now();
    let mut remote = remote(at);
    let packets = packets();
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    writers[0].control.active.store(true, Ordering::Release);
    remote.push(0, 0, packets[0].clone(), at, at);
    remote.push(1, 6000, packets[1].clone(), at, at);
    remote.refill(&mut writers[0], at + Duration::from_millis(40));
    assert_eq!(writers[0].queue.occupied_len(), FRAME);
    remote.refill(&mut writers[0], at + Duration::from_millis(45));
    assert!(!writers[0].ready());
    let mut output = [1.0; FRAME];
    mixer.render(&mut output, 1);
    assert_eq!(output, [0.0; FRAME]);
    assert!(writers[0].ready());
    assert_eq!(remote.jitter.metrics().rebufferings, 1);
}

#[test]
fn responsive_worker_expires_decoded_pcm_when_output_stops_without_more_packets() {
    let at = Instant::now();
    let mut remote = remote(at);
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    writers[0].control.active.store(true, Ordering::Release);
    remote.push(0, 0, packets()[0].clone(), at, at);
    for ms in (0..205).step_by(5) {
        remote.refill(&mut writers[0], at + Duration::from_millis(ms));
        if ms == 40 {
            assert_eq!(writers[0].queue.occupied_len(), FRAME);
        }
    }
    assert!(!writers[0].ready());
    let mut output = [1.0; FRAME];
    mixer.render(&mut output, 1);
    assert_eq!(output, [0.0; FRAME]);
    assert!(writers[0].ready());
    assert_eq!(remote.jitter.metrics().worker_stalls, 0);
}

#[test]
fn deafen_unread_packets_and_source_replacement_cannot_replay_previous_pcm() {
    let at = Instant::now();
    let packets = packets();
    let mut speaker = remote(at);
    let (mut writers, mut mixer) = mixer(Arc::new(Controls::default()));
    writers[0].control.active.store(true, Ordering::Release);
    for (i, payload) in packets.iter().take(3).enumerate() {
        speaker.push(i as u16, i as u32 * 960, payload.clone(), at, at);
    }
    speaker.refill(&mut writers[0], at + Duration::from_millis(40));
    assert!(speaker.activity.speaking(at + Duration::from_millis(40)));
    let now = at + Duration::from_millis(70);
    speaker.set_deafened(&writers[0], true, now);
    speaker.push(3, 2880, packets[3].clone(), now, now);
    speaker.set_deafened(&writers[0], false, now);
    // A packet received before the control transition cannot get a new lifetime.
    speaker.push(4, 3840, packets[4].clone(), at, now);
    let mut output = [1.0; FRAME];
    mixer.render(&mut output, 1);
    speaker.refill(&mut writers[0], now);
    mixer.render(&mut output, 1);
    assert_eq!(output, [0.0; FRAME]);
    assert!(!speaker.activity.speaking(now));
    // New source/connection owns a fresh decoder and jitter clock, but old media
    // which was already in the local dispatch queue is fenced by arrival time.
    let mut replacement = remote(now);
    writers[0].discard();
    replacement.push(5, 4800, packets[5].clone(), at, now);
    mixer.render(&mut output, 1);
    replacement.refill(&mut writers[0], now + Duration::from_millis(60));
    mixer.render(&mut output, 1);
    assert_eq!(output, [0.0; FRAME]);
}

#[test]
fn resetting_one_stream_preserves_other_stream_pcm_and_the_final_echo_reference() {
    let controls = Arc::new(Controls::default());
    let (mut writers, mixer) = mixer(controls);
    let (reference, mut rendered) = frames::queue();
    let mut mixer = mixer.with_reference(reference);
    for writer in &writers[..2] {
        writer.control.active.store(true, Ordering::Release);
    }
    writers[0].write(&[0.3; 480]);
    writers[1].write(&[0.1; 480]);
    writers[0].discard();
    let mut output = [0.0; 480];
    let at = Instant::now();
    mixer.reference_time(at);
    mixer.render(&mut output, 1);
    assert_eq!(output, [0.1; 480]);
    assert_eq!(rendered.pop().unwrap().0.samples, output);
    assert!(writers[0].ready());
}

#[test]
fn unsupported_packet_duration_and_invalid_payload_do_not_advance_receive_clock() {
    let at = Instant::now();
    let mut speaker = remote(at);
    let mut encoder =
        opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
    let mut bytes = [0; 1500];
    let len = encoder.encode_float(&[0.1; 480], &mut bytes).unwrap();
    speaker.push(0, 0, bytes::Bytes::copy_from_slice(&bytes[..len]), at, at);
    speaker.push(1, 960, bytes::Bytes::new(), at, at);
    assert!(
        speaker
            .jitter
            .pop(true, at + Duration::from_secs(1))
            .is_none()
    );
}
