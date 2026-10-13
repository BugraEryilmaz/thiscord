//! Production Opus -> scheduling -> rate correction -> ring -> mix -> AEC path.
use super::jitter::legacy;
use super::*;

fn remote() -> Remote {
    Remote {
        since: Instant::now(),
        deafened: false,
        decode_stats: Default::default(),
        activity: Default::default(),
        id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        label: "fixture".into(),
        jitter: Default::default(),
        decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        rate: Default::default(),
    }
}

fn packets() -> (Vec<bytes::Bytes>, Vec<f32>) {
    let mut encoder =
        opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Audio).unwrap();
    let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    let mut rate = playout_rate::Rate::default();
    let mut packets = Vec::new();
    let mut expected = Vec::new();
    for seq in 0..60 {
        let pcm = std::array::from_fn::<_, FRAME, _>(|i| {
            ((seq * FRAME + i) as f32 * std::f32::consts::TAU * 440.0 / RATE as f32).sin() * 0.1
        });
        let mut encoded = [0; 1500];
        let n = encoder.encode_float(&pcm, &mut encoded).unwrap();
        packets.push(bytes::Bytes::copy_from_slice(&encoded[..n]));
        let mut decoded = [0.0; FRAME];
        assert_eq!(
            decoder
                .decode_float(&encoded[..n], &mut decoded, false)
                .unwrap(),
            FRAME
        );
        let mut corrected = [0.0; playout_rate::MAX_OUTPUT];
        let n = rate.process(&decoded, 0.0, &mut corrected);
        expected.extend_from_slice(&corrected[..n]);
    }
    (packets, expected)
}

#[test]
fn opus_continuity_through_irregular_worker_wakes_and_final_aec_reference() {
    let at = Instant::now();
    let (packets, expected) = packets();
    let controls = Arc::new(Controls::default());
    let (mut writers, mixer) = mixer::mixer(controls.clone());
    let (reference_writer, mut reference) = frames::queue();
    let mut mixer = mixer.with_reference(reference_writer);
    let writer = &mut writers[0];
    writer.control.active.store(true, Ordering::Release);
    writer.volume(1.5).unwrap();
    controls.master.store(0.5_f32.to_bits(), Ordering::Relaxed);
    let mut remote = remote();
    let mut actual = Vec::new();
    let mut next_worker = 40;
    let mut wake = 0;
    for ms in 0..1040 {
        let now = at + Duration::from_millis(ms as u64);
        for (seq, packet) in packets.iter().enumerate() {
            if ms == seq * 20 {
                remote.jitter.push_packet(
                    jitter::Packet {
                        marker: false,
                        no_data: false,
                        sequence: seq as u16,
                        timestamp: seq as u32 * 960,
                        arrived: now,
                        payload: packet.clone(),
                    },
                    now,
                );
            }
        }
        if ms >= next_worker {
            remote.refill(writer, now);
            next_worker += [5, 7, 12, 4, 8, 5][wake % 6];
            wake += 1;
        }
        if ms == 40 {
            controls.underruns.store(0, Ordering::Relaxed);
        }
        let mut output = [0.0; 48];
        mixer.reference_time(now);
        mixer.render(&mut output, 1);
        actual.extend_from_slice(&output);
        if ms % 10 == 9 {
            let (frame, gap) = reference.pop().unwrap();
            assert!(!gap);
            assert_eq!(frame.samples, actual[actual.len() - 480..]);
            assert_eq!(frame.at, at + Duration::from_millis((ms - 9) as u64));
        }
    }
    assert_eq!(remote.jitter.metrics().concealments, 0);
    assert_eq!(controls.underruns.load(Ordering::Relaxed), 0);
    assert!(actual[..40 * 48].iter().all(|v| *v == 0.0));
    // No drift on this trace: even with irregular worker wakes the production
    // path equals an independently decoded, continuously filtered stream.
    let wanted: Vec<_> = expected[..48000].iter().map(|v| v * 1.5 * 0.5).collect();
    assert_eq!(&actual[40 * 48..], wanted);
}

#[test]
fn independent_sources_correction_deafen_and_generation_replacement() {
    let controls = Arc::new(Controls::default());
    let (mut writers, mixer) = mixer::mixer(controls.clone());
    let (reference_writer, mut reference) = frames::queue();
    let mut mixer = mixer.with_reference(reference_writer);
    let mut rates = [playout_rate::Rate::default(), playout_rate::Rate::default()];
    let mut outputs = [[0.0; playout_rate::MAX_OUTPUT]; 2];
    for (i, ppm) in [-400.0, 400.0].into_iter().enumerate() {
        writers[i].control.active.store(true, Ordering::Release);
        let input: [f32; FRAME] = std::array::from_fn(|sample| (sample as f32 * 0.02).sin() * 0.1);
        let count = rates[i].process(&input, ppm, &mut outputs[i]);
        writers[i].write(&outputs[i][..count]);
    }
    let mut out = [0.0; 480];
    mixer.render(&mut out, 1);
    for (i, sample) in out.iter().enumerate() {
        assert_eq!(*sample, outputs[0][i] + outputs[1][i]);
    }
    assert_eq!(reference.pop().unwrap().0.samples, out);
    controls.deafen.store(true, Ordering::Release);
    mixer.render(&mut out, 1);
    assert_eq!(out, [0.0; 480]);
    assert_eq!(reference.pop().unwrap().0.samples, out);
    controls.deafen.store(false, Ordering::Release);
    writers[0].write(&[0.5; 480]);
    writers[0].control.generation.fetch_add(1, Ordering::AcqRel);
    mixer.render(&mut out, 1);
    assert_eq!(out, [0.0; 480]);
    let mut replacement = remote();
    let at = Instant::now();
    replacement.refill(&mut writers[0], at);
    assert_eq!(writers[0].queue.occupied_len(), 0);
    assert_eq!(replacement.jitter.metrics().packets, 0);
}

#[test]
fn simultaneous_speakers_keep_network_estimates_and_decoders_independent() {
    let at = Instant::now();
    let (packets, _) = packets();
    let controls = Arc::new(Controls::default());
    let (mut writers, mut mixer) = mixer::mixer(controls.clone());
    let mut remotes = [remote(), remote()];
    for writer in writers.iter().take(2) {
        writer.control.active.store(true, Ordering::Release);
    }
    for ms in 0..1040 {
        let now = at + Duration::from_millis(ms as u64);
        for (source, remote) in remotes.iter_mut().enumerate() {
            for (seq, payload) in packets.iter().enumerate() {
                let jitter = if source == 0 { 0 } else { (seq * 7) % 24 };
                if ms == seq * 20 + jitter {
                    remote.jitter.push_packet(
                        jitter::Packet {
                            marker: false,
                            no_data: false,
                            sequence: seq as u16,
                            timestamp: seq as u32 * 960,
                            arrived: now,
                            payload: payload.clone(),
                        },
                        now,
                    );
                }
            }
            if ms % 5 == 0 {
                remote.refill(&mut writers[source], now);
            }
        }
        if ms == 45 {
            controls.underruns.store(0, Ordering::Relaxed);
        }
        mixer.render(&mut [0.0_f32; 48], 1);
    }
    assert_eq!(remotes[0].jitter.metrics().target_ms, 40.0);
    assert!(remotes[1].jitter.metrics().target_ms > 45.0);
    assert_eq!(controls.underruns.load(Ordering::Relaxed), 0);
    for remote in remotes {
        assert_eq!(remote.jitter.metrics().concealments, 0);
        assert_eq!(remote.jitter.metrics().decode_errors, 0);
    }
}

#[test]
fn unsupported_opus_duration_keeps_one_bounded_timeline_slot() {
    let at = Instant::now();
    let mut encoder =
        opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Audio).unwrap();
    let mut packet = [0; 1500];
    let size = encoder.encode_float(&[0.0; 480], &mut packet).unwrap();
    let mut remote = remote();
    let (mut writers, _) = mixer::mixer(Arc::new(Controls::default()));
    remote.jitter.push_packet(
        jitter::Packet {
            marker: false,
            no_data: false,
            sequence: 0,
            timestamp: 0,
            arrived: at,
            payload: bytes::Bytes::copy_from_slice(&packet[..size]),
        },
        at,
    );
    remote.refill(&mut writers[0], at + Duration::from_millis(40));
    assert_eq!(writers[0].queue.occupied_len(), FRAME);
    assert_eq!(remote.jitter.metrics().decode_errors, 1);
}

#[test]
fn fec_carrier_is_decoded_again_normally_without_prefetch_plc() {
    let at = Instant::now();
    let (packets, _) = packets();
    let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    let mut rate = playout_rate::Rate::default();
    let mut expected = Vec::new();
    for (seq, fec) in [(0, false), (2, true), (2, false)] {
        let mut pcm = [0.0; FRAME];
        assert_eq!(
            decoder.decode_float(&packets[seq], &mut pcm, fec).unwrap(),
            FRAME
        );
        let mut corrected = [0.0; playout_rate::MAX_OUTPUT];
        let n = rate.process(&pcm, 0.0, &mut corrected);
        expected.extend_from_slice(&corrected[..n]);
    }
    let controls = Arc::new(Controls::default());
    let (mut writers, mut mixer) = mixer::mixer(controls);
    writers[0].control.active.store(true, Ordering::Release);
    let mut remote = remote();
    let mut actual = Vec::new();
    for ms in 0..100 {
        let now = at + Duration::from_millis(ms);
        if ms == 0 || ms == 40 {
            let seq = (ms / 20) as usize;
            remote.jitter.push_packet(
                jitter::Packet {
                    marker: false,
                    no_data: false,
                    sequence: seq as u16,
                    timestamp: seq as u32 * 960,
                    arrived: now,
                    payload: packets[seq].clone(),
                },
                now,
            );
        }
        if ms % 5 == 0 {
            remote.refill(&mut writers[0], now);
        }
        let mut out = [0.0_f32; 48];
        mixer.render(&mut out, 1);
        if ms >= 40 {
            actual.extend_from_slice(&out);
        }
    }
    assert_eq!(remote.jitter.metrics().fec_attempts, 1);
    assert_eq!(actual, expected);
}

/// Optional WAVs are generated only from the committed synthetic speech fixture.
/// No device/network access or recording of conversations takes place.
#[test]
fn speech_trace_comparison() {
    let speech = fixture::speech();
    let mut encoder =
        opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Audio).unwrap();
    let mut packets = Vec::new();
    let mut events = Vec::new();
    for seq in 0..800 {
        let pcm = std::array::from_fn::<_, FRAME, _>(|i| speech[(seq * FRAME + i) % speech.len()]);
        let mut encoded = [0; 1500];
        let n = encoder.encode_float(&pcm, &mut encoded).unwrap();
        packets.push(bytes::Bytes::copy_from_slice(&encoded[..n]));
        if !(500..506).contains(&seq) {
            let delay = if (200..400).contains(&seq) && seq % 10 < 3 {
                80
            } else {
                0
            };
            events.push((seq * 20 + delay, seq));
        }
    }
    events.sort_unstable();
    let at = Instant::now();
    for old in [true, false] {
        let controls = Arc::new(Controls::default());
        let (mut writers, mut mixer) = mixer::mixer(controls.clone());
        writers[0].control.active.store(true, Ordering::Release);
        let mut remote = remote();
        let mut legacy = legacy::Legacy::default();
        let mut event = 0;
        let mut output = Vec::new();
        let mut concealments = 0;
        for ms in 0..16000 {
            let now = at + Duration::from_millis(ms as u64);
            while event < events.len() && events[event].0 <= ms {
                let seq = events[event].1;
                if old {
                    legacy.push(seq as u16, packets[seq].clone(), now);
                } else {
                    remote.jitter.push_packet(
                        jitter::Packet {
                            marker: false,
                            no_data: false,
                            sequence: seq as u16,
                            timestamp: seq as u32 * 960,
                            arrived: now,
                            payload: packets[seq].clone(),
                        },
                        now,
                    );
                }
                event += 1;
            }
            if ms % 5 == 0 {
                if old {
                    for _ in 0..(FRAME * 2)
                        .saturating_sub(writers[0].queue.occupied_len())
                        .div_ceil(FRAME)
                    {
                        let Some(packet) =
                            legacy.pop(writers[0].queue.occupied_len() <= FRAME / 2, now)
                        else {
                            break;
                        };
                        concealments += u64::from(packet.is_none());
                        let mut pcm = [0.0; FRAME];
                        if let Ok(FRAME) = remote.decoder.decode_float(
                            packet.as_deref().unwrap_or(&[]),
                            &mut pcm,
                            false,
                        ) {
                            writers[0].write(&pcm);
                        }
                    }
                } else {
                    remote.refill(&mut writers[0], now);
                }
            }
            if ms == 40 {
                controls.underruns.store(0, Ordering::Relaxed);
            }
            let mut rendered = [0.0_f32; 48];
            mixer.render(&mut rendered, 1);
            output.extend_from_slice(&rendered);
        }
        if !old {
            concealments = remote.jitter.metrics().concealments;
        }
        let peak_step = output
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0_f32, f32::max);
        let name = if old { "legacy" } else { "adaptive" };
        println!(
            "speech {name}: concealments={concealments}, underrun_samples={}, peak_adjacent_step={peak_step:.6}",
            controls.underruns.load(Ordering::Relaxed)
        );
        assert!(output.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        assert!(fixture::energy(&output) > 1.0);
        if let Some(directory) = std::env::var_os("THISCORD_PLAYOUT_FIXTURE_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            let path = std::path::PathBuf::from(directory).join(format!("playout-{name}.wav"));
            let mut wav = hound::WavWriter::create(
                path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for sample in output {
                wav.write_sample((sample * 32767.0) as i16).unwrap();
            }
            wav.finalize().unwrap();
        }
    }
}
