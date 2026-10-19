use super::*;
use crate::audio::{
    codec, fixture,
    mixer::{FRAME, RATE},
    transmit,
};

fn sample(start: Instant, second: u64, sequence: u32, lost: u32) -> Sample {
    Sample {
        source: 1,
        generation: 0,
        reports: second + 1,
        sequence,
        lost,
        at: start + Duration::from_secs(second),
    }
}
fn warmed(start: Instant) -> (Controller, Sample) {
    let mut c = Controller::default();
    let mut s = sample(start, 0, 100, 0);
    for second in 0..20 {
        s = sample(start, second, 100 + second as u32 * 50, second as u32 * 10);
        c.update(true, 5, Some(s), s.at);
    }
    assert!((18..=20).contains(&c.hint));
    (c, s)
}

#[test]
fn disabled_toggle_staleness_and_source_replacement() {
    let start = Instant::now();
    let (mut c, s) = warmed(start);
    assert!(c.diagnostics(s.at).adapting);
    // Repeated get_stats snapshots cannot refresh an old RR.
    assert_eq!(c.update(true, 5, Some(s), s.at + STALE), 5);
    assert!(!c.diagnostics(s.at + STALE).adapting);
    let (mut c, s) = warmed(start);
    assert_eq!(c.update(false, 5, Some(s), s.at), 5);
    assert!(!c.diagnostics(s.at).enabled);
    assert_eq!(c.update(true, 5, Some(s), s.at), 5);
    let (mut c, mut s) = warmed(start);
    s.source = 2;
    assert_eq!(c.update(true, 5, Some(s), s.at), 5);
    assert_eq!(c.update(true, 5, None, s.at), 5);
    let mut fixed = Controller::default();
    for second in 0..30 {
        let s = sample(start, second, second as u32 * 50, second as u32 * 20);
        assert_eq!(fixed.update(false, 5, Some(s), s.at), 5);
    }
}

#[test]
fn intentional_privacy_pause_has_no_loss_and_preserves_recent_path_estimate() {
    let start = Instant::now();
    let (mut c, last) = warmed(start);
    let previous = c.hint;
    // Several seconds of an acknowledged privacy boundary/DTX do not imply
    // missing RTP. Fresh reports with unchanged sequence have no loss sample.
    for second in 20..23 {
        let paused = sample(start, second, last.sequence, last.lost);
        assert_eq!(c.update(true, 5, Some(paused), paused.at), previous);
    }
    let resumed = sample(start, 23, last.sequence + 50, last.lost);
    assert_eq!(c.update(true, 5, Some(resumed), resumed.at), previous - 1);
    // A real stable source replacement, even on the same peer, must rebaseline.
    let replacement = Sample {
        generation: 1,
        ..resumed
    };
    assert_eq!(c.update(true, 5, Some(replacement), replacement.at), 5);
}

#[test]
fn small_samples_dtx_counter_resets_reordering_and_wrap_are_bounded() {
    let start = Instant::now();
    for sequence in [65_500_u32, u32::MAX - 20] {
        let mut c = Controller::default();
        let a = sample(start, 0, sequence, 0);
        let b = sample(start, 1, sequence.wrapping_add(50), 10);
        c.update(true, 5, Some(a), a.at);
        assert_eq!(c.update(true, 5, Some(b), b.at), 8);
        let reset = sample(start, 2, 0, 0);
        assert_eq!(c.update(true, 5, Some(reset), reset.at), 5);
    }
    let mut c = Controller::default();
    for second in 0..40 {
        // Silence/CN packets advance sequence by only two, regardless of the
        // 50 audio frames omitted per second. No manufactured loss denominator.
        let s = sample(start, second, second as u32 * 2, 0);
        assert_eq!(c.update(true, 5, Some(s), s.at), 5);
    }
    let (mut c, s) = warmed(start);
    for (sequence, lost, reports) in [
        (s.sequence - 50, s.lost - 10, 21),
        (u32::MAX, 0, 22),
        (1, 1, 1),
    ] {
        let s = Sample {
            sequence,
            lost,
            reports,
            at: s.at + Duration::from_secs(1),
            ..s
        };
        assert_eq!(c.update(true, 5, Some(s), s.at), 5);
    }
    let mut c = Controller::default();
    for second in 0..60 {
        let s = sample(start, second, second as u32 * 50, second as u32 * 50);
        let before = c.hint;
        let hint = c.update(true, 5, Some(s), s.at);
        assert!(hint <= 30 && hint <= before + 3);
    }
    assert!(c.hint >= 28);
}

#[test]
fn high_report_rate_and_impossible_loss_do_not_drive_encoder() {
    let start = Instant::now();
    let mut c = Controller::default();
    for report in 0..100 {
        let s = Sample {
            at: start + Duration::from_millis(report),
            ..sample(start, report, report as u32, report as u32)
        };
        assert_eq!(c.update(true, 5, Some(s), s.at), 5);
    }
    let impossible = sample(start, 2, 100, 101);
    assert_eq!(c.update(true, 5, Some(impossible), impossible.at), 5);
    assert!(!c.diagnostics(impossible.at).adapting);
}

#[test]
fn floor_precedence_survives_low_high_absent_stale_feedback_and_rapid_toggles() {
    let start = Instant::now();
    let mut encoder = transmit::Encoder::new(false).unwrap();
    for floor in [5, 20, 5, 20, 20, 5] {
        for enabled in [false, true, false, true] {
            for loss in [0, 10, 20] {
                encoder.adapt(enabled, floor, None, start).unwrap();
                assert_eq!(
                    encoder.codec.get_packet_loss_perc().unwrap(),
                    i32::from(floor)
                );
                for second in 0..20 {
                    let s = sample(start, second, second as u32 * 50, second as u32 * loss);
                    encoder.adapt(enabled, floor, Some(s), s.at).unwrap();
                    let hint = encoder.codec.get_packet_loss_perc().unwrap();
                    assert!((i32::from(floor)..=30).contains(&hint));
                    if !enabled {
                        assert_eq!(hint, i32::from(floor));
                    }
                }
                encoder.reset().unwrap();
                encoder
                    .adapt(
                        enabled,
                        floor,
                        Some(sample(start, 19, 950, 19 * loss)),
                        start + Duration::from_secs(30),
                    )
                    .unwrap();
                assert_eq!(
                    encoder.codec.get_packet_loss_perc().unwrap(),
                    i32::from(floor)
                );
            }
        }
    }
}

#[test]
fn rtcp_mailbox_bounds_replay_rate_sequence_and_generation() {
    use rtc::rtcp::reception_report::ReceptionReport;
    let start = Instant::now();
    let f = Feedback::default();
    let report = |sequence, lost| ReceptionReport {
        ssrc: MediaKind::Microphone.publisher_ssrc(),
        last_sequence_number: sequence,
        total_lost: lost,
        ..Default::default()
    };
    f.observe(1, &report(50, 10), start);
    assert!(f.sample(0).is_none()); // no local transmission yet
    f.sent(0, 1);
    f.sent(0, 100);
    let mut wrong = report(50, 10);
    wrong.ssrc = MediaKind::SystemAudio.publisher_ssrc();
    f.observe(1, &wrong, start);
    f.observe(1, &report(101, 10), start); // claims future packets
    f.observe(1, &report(50, 0xff_ffff), start);
    assert!(f.sample(0).is_none());
    f.observe(1, &report(50, 10), start);
    for n in 0..10_000 {
        f.observe(1, &report(60, 12), start + Duration::from_nanos(n));
    }
    assert_eq!(f.sample(0).unwrap().sequence, 50);
    f.observe(1, &report(50, 10), start + Duration::from_secs(10));
    assert_eq!(f.sample(0).unwrap().at, start); // duplicate cannot refresh
    f.observe(1, &report(40, 8), start + Duration::from_secs(10));
    assert_eq!(f.sample(0).unwrap().sequence, 50); // reordered report rejected
    f.observe(1, &report(90, 18), start + Duration::from_secs(11));
    assert_eq!(f.sample(0).unwrap().sequence, 90);
    assert!(f.sample(1).is_none()); // stable source fence before first send
    f.sent(1, 101);
    f.sent(0, 100); // late prior-source completion cannot undo fence
    f.observe(1, &report(90, 18), start + Duration::from_secs(12));
    assert!(f.sample(1).is_none());
    f.sent(1, 151);
    f.observe(1, &report(140, 28), start + Duration::from_secs(13));
    assert_eq!(f.sample(1).unwrap().generation, 1);
    assert!(f.sample(0).is_none());
    assert!(std::mem::size_of::<Feedback>() < 256);
}

#[test]
fn real_encoder_live_toggle_and_privacy_reset_preserve_budget_and_clear_history() {
    let start = Instant::now();
    let mut enc = transmit::Encoder::new(true).unwrap();
    for second in 0..20 {
        let s = sample(start, second, second as u32 * 50, second as u32 * 10);
        enc.adapt(true, 5, Some(s), s.at).unwrap();
    }
    assert!(enc.codec.get_packet_loss_perc().unwrap() >= 18);
    assert_eq!(
        enc.codec.get_bitrate().unwrap(),
        opus::Bitrate::Bits(32_000)
    );
    let speech = fixture::speech();
    let mut buffer = [0; 4000];
    for pcm in speech.as_chunks::<FRAME>().0.iter().take(40) {
        enc.encode(pcm, true, &mut buffer).unwrap();
    }
    enc.reset().unwrap();
    // Same CTLs, fresh codec: privacy reset must erase redundant speech too.
    let mut fresh = transmit::Encoder::new(true).unwrap();
    fresh
        .codec
        .set_packet_loss_perc(enc.codec.get_packet_loss_perc().unwrap())
        .unwrap();
    let mut reference = [0; 4000];
    for _ in 0..25 {
        let (n, omitted, _) = enc.encode(&[0.0; FRAME], false, &mut buffer).unwrap();
        let (r, expected, _) = fresh.encode(&[0.0; FRAME], false, &mut reference).unwrap();
        assert_eq!((n, omitted), (r, expected));
        assert_eq!(&buffer[..n], &reference[..r]);
    }
    enc.adapt(false, 5, None, start + Duration::from_secs(21))
        .unwrap();
    assert_eq!(enc.codec.get_packet_loss_perc().unwrap(), 5);
    assert!(enc.codec.get_inband_fec().unwrap());
}

#[test]
fn deterministic_isolated_and_burst_loss_measure_real_opus_fec() {
    let speech = fixture::speech();
    for enabled in [false, true] {
        let start = Instant::now();
        let mut encoder = codec::microphone_encoder().unwrap();
        let hint = if enabled { warmed(start).0.hint } else { 5 };
        encoder.set_packet_loss_perc(i32::from(hint)).unwrap();
        let delay = encoder.get_lookahead().unwrap() as usize;
        let packets: Vec<Vec<u8>> = speech
            .as_chunks::<FRAME>()
            .0
            .iter()
            .map(|pcm| {
                let mut output = [0; 4000];
                let n = encoder.encode_float(pcm, &mut output).unwrap();
                output[..n].to_vec()
            })
            .collect();
        let bytes: usize = packets.iter().map(Vec::len).sum();
        for burst in [false, true] {
            let missing = |i: usize| {
                i > 10
                    && if burst {
                        i % 15 < 3
                    } else {
                        i.is_multiple_of(5)
                    }
            };
            let mut fec = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
            let mut plc = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
            let (mut lost, mut opportunities, mut distinct) = (0, 0, 0);
            let (mut fec_error, mut plc_error) = (0.0, 0.0);
            for i in 0..packets.len() - 1 {
                let gone = missing(i);
                let borrow = gone && !missing(i + 1);
                let mut a = [0.0; FRAME];
                let mut b = [0.0; FRAME];
                fec.decode_float(
                    if borrow {
                        &packets[i + 1]
                    } else if gone {
                        &[]
                    } else {
                        &packets[i]
                    },
                    &mut a,
                    borrow,
                )
                .unwrap();
                plc.decode_float(if gone { &[] } else { &packets[i] }, &mut b, false)
                    .unwrap();
                if gone {
                    lost += 1;
                    opportunities += usize::from(borrow);
                    distinct +=
                        usize::from(borrow && a.iter().zip(b).any(|(x, y)| (x - y).abs() > 1e-5));
                    for ((x, y), z) in speech[i * FRAME - delay..(i + 1) * FRAME - delay]
                        .iter()
                        .zip(a)
                        .zip(b)
                    {
                        fec_error += f64::from(x - y).powi(2);
                        plc_error += f64::from(x - z).powi(2);
                    }
                }
                assert!(a.iter().all(|x| x.is_finite()));
            }
            assert!(distinct > 5 && opportunities <= lost);
            assert!(
                fec_error < plc_error,
                "synthetic fixture FEC must improve on PLC"
            );
            if burst {
                assert!(opportunities * 2 < lost);
            }
            eprintln!(
                "adaptive={enabled} hint={hint}% burst={burst}: loss={lost}, successor opportunities={opportunities}, outputs distinct from PLC={distinct}; squared error FEC={fec_error:.3} PLC={plc_error:.3}; payload={bytes} bytes, payload rate={:.2} kbit/s, codec lookahead={:.2} ms, added buffering=0 ms",
                bytes as f64 * 8.0 / (packets.len() as f64 * 20.0),
                delay as f64 / 48.0
            );
        }
    }
}
