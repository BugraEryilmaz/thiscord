use super::*;

fn packet(sequence: u16, timestamp: u32, arrived: Instant) -> Packet {
    Packet {
        sequence,
        timestamp,
        arrived,
        payload: bytes::Bytes::from_static(b"packet"),
    }
}

fn push(j: &mut Jitter, sequence: u16, timestamp: u32, at: Instant) {
    j.push(packet(sequence, timestamp, at), at);
}

#[test]
fn deadlines_reordering_fec_duplicates_and_both_wraps() {
    let at = Instant::now();
    let ts = u32::MAX - 959;
    let mut j = Jitter::default();
    push(&mut j, 65534, ts, at);
    push(
        &mut j,
        0,
        ts.wrapping_add(1920),
        at + Duration::from_millis(39),
    );
    push(
        &mut j,
        0,
        ts.wrapping_add(1920),
        at + Duration::from_millis(39),
    );
    assert!(j.poll(at + Duration::from_millis(39), 0).is_none());
    assert!(matches!(
        j.poll(at + Duration::from_millis(40), 0).unwrap().decode,
        Decode::Packet(_)
    ));
    for ms in 40..55 {
        assert!(
            j.poll(
                at + Duration::from_millis(ms),
                960 - (ms as usize - 40) * 48
            )
            .is_none()
        );
    }
    // 5 ms from playback: FEC may now resolve the missing frame, but its
    // carrier remains queued and is normally decoded exactly once afterward.
    assert!(matches!(
        j.poll(at + Duration::from_millis(55), 240).unwrap().decode,
        Decode::Fec(_)
    ));
    assert!(matches!(
        j.poll(at + Duration::from_millis(75), 240).unwrap().decode,
        Decode::Packet(_)
    ));
    push(
        &mut j,
        65535,
        ts.wrapping_add(960),
        at + Duration::from_millis(76),
    );
    assert_eq!(j.metrics().late_discards, 1);
    assert_eq!(j.metrics().duplicates, 1);
    assert_eq!(j.metrics().fec_attempts, 1);
}

#[test]
fn late_but_before_pcm_deadline_avoids_concealment() {
    let at = Instant::now();
    let mut j = Jitter::default();
    for seq in [0, 2, 3] {
        push(&mut j, seq, u32::from(seq) * 960, at);
    }
    assert!(j.poll(at + Duration::from_millis(40), 0).is_some());
    assert!(j.poll(at + Duration::from_millis(50), 480).is_none());
    push(&mut j, 1, 960, at + Duration::from_millis(54));
    assert!(matches!(
        j.poll(at + Duration::from_millis(54), 288).unwrap().decode,
        Decode::Packet(_)
    ));
    assert_eq!(j.metrics().concealments, 0);
}

#[test]
fn silence_and_timestamp_gaps_do_not_become_unbounded_loss() {
    let at = Instant::now();
    let mut j = Jitter::default();
    push(&mut j, 0, 0, at);
    j.poll(at + Duration::from_millis(40), 0).unwrap();
    // Consecutive sequence with a short timestamp gap is explicit DTX evidence.
    push(&mut j, 1, 4800, at + Duration::from_millis(100));
    assert!(j.poll(at + Duration::from_millis(100), 0).is_none());
    assert!(j.poll(at + Duration::from_millis(140), 0).unwrap().reset);
    assert_eq!(j.metrics().concealments, 0);
    for ms in [160, 180, 200, 220, 240, 260] {
        j.poll(at + Duration::from_millis(ms), 0);
    }
    assert_eq!(j.metrics().concealments, 3);
    assert!(j.metrics().inactive);
    push(&mut j, 2, 48000, at + Duration::from_secs(1));
    assert!(j.poll(at + Duration::from_millis(1040), 0).unwrap().reset);
}

#[test]
fn worker_queue_delay_is_not_network_jitter_and_stalls_rebuffer() {
    let at = Instant::now();
    let mut a = Jitter::default();
    let mut b = Jitter::default();
    for seq in 0..6 {
        let arrival = at + Duration::from_millis(u64::from(seq) * 20);
        a.push(packet(seq, u32::from(seq) * 960, arrival), arrival);
        b.push(
            packet(seq, u32::from(seq) * 960, arrival),
            arrival + Duration::from_millis(80),
        );
    }
    assert_eq!(a.metrics().variation_ms, b.metrics().variation_ms);
    assert_eq!(a.metrics().target_ms, b.metrics().target_ms);
    b.poll(at + Duration::from_millis(180), 0).unwrap();
    assert!(b.poll(at + Duration::from_millis(400), 0).is_none());
    assert_eq!(b.metrics().worker_stalls, 1);
    b.push(packet(10, 9600, at), at + Duration::from_secs(1));
    assert_eq!(b.metrics().worker_queue_discards, 1);
    assert_eq!(b.metrics().variation_ms, 0.0);
}

#[test]
fn stalled_output_discards_stale_packets_without_inflating_network_variation() {
    let at = Instant::now();
    let mut j = Jitter::default();
    for seq in 0..4 {
        push(
            &mut j,
            seq,
            u32::from(seq) * 960,
            at + Duration::from_millis(u64::from(seq) * 20),
        );
    }
    j.poll(at + Duration::from_millis(60), 0).unwrap();
    // Worker remains responsive but output stops consuming. The ring remains
    // bounded, then the scheduler refuses old speech once the device resumes.
    for ms in (65..400).step_by(5) {
        assert!(j.poll(at + Duration::from_millis(ms), 960).is_none());
    }
    assert!(j.poll(at + Duration::from_millis(400), 0).is_none());
    assert_eq!(j.metrics().stale_discards, 3);
    assert_eq!(j.metrics().worker_stalls, 0);
    assert_eq!(j.metrics().variation_ms, 0.0);
}

#[test]
fn bounded_capacity_payloads_and_forward_discontinuity() {
    let at = Instant::now();
    let mut j = Jitter::default();
    for seq in 0..64 {
        push(&mut j, seq, u32::from(seq) * 960, at);
    }
    assert_eq!(j.metrics().packets, MAX_PACKETS);
    assert_eq!(j.metrics().capacity_discards, 64 - MAX_PACKETS as u64);
    push(&mut j, 1000, 960000, at);
    assert_eq!(j.metrics().packets, 1);
    assert_eq!(j.metrics().discontinuities, 1);
    let mut invalid = packet(1001, 960960, at);
    invalid.payload = bytes::Bytes::from(vec![0; 1501]);
    j.push(invalid, at);
    assert_eq!(j.metrics().packets, 1);
    assert!(j.poll(at + Duration::from_millis(120), 0).unwrap().reset);
}

#[derive(Default, Debug)]
struct Report {
    underrun_ms: usize,
    max_pcm: f64,
    max_delay: f64,
    max_target: f64,
    max_correction: f64,
    final_delay: f64,
    final_correction: f64,
    concealments: u64,
    rebufferings: u64,
}

/// Deterministic receiver/worker/device clocks. Payload decoding is deliberately
/// abstract here; separate integration tests exercise Opus, sinc and the mixer.
fn trace(
    seconds: usize,
    sender_ppm: f64,
    output_ppm: f64,
    network: impl Fn(usize) -> Option<usize>,
) -> Report {
    run_trace(seconds, sender_ppm, output_ppm, network, false)
}

fn run_trace(
    seconds: usize,
    sender_ppm: f64,
    output_ppm: f64,
    network: impl Fn(usize) -> Option<usize>,
    old: bool,
) -> Report {
    let at = Instant::now();
    let mut events: Vec<_> = (0..seconds * 51)
        .filter_map(|i| {
            network(i).map(|jitter| {
                (
                    ((i as f64 * 20.0 / (1.0 + sender_ppm / 1e6)).round() as usize) + jitter,
                    i,
                )
            })
        })
        .collect();
    events.sort_unstable();
    let mut j = Jitter::default();
    let mut legacy = legacy::Legacy::default();
    let mut event = 0;
    let mut pcm = 0.0_f64;
    let mut started = false;
    let mut report = Report::default();
    for ms in 0..seconds * 1000 {
        let now = at + Duration::from_millis(ms as u64);
        while event < events.len() && events[event].0 <= ms {
            let i = events[event].1;
            push(
                &mut j,
                i as u16,
                (i as u32).wrapping_mul(960).wrapping_add(u32::MAX - 959),
                now,
            );
            if old {
                legacy.push(
                    i as u16,
                    bytes::Bytes::copy_from_slice(&(ms as u64).to_le_bytes()),
                    now,
                );
            }
            event += 1;
        }
        if ms % 5 == 0 {
            for _ in 0..2 {
                if old {
                    if pcm >= 1920.0 {
                        break;
                    }
                    let Some(packet) = legacy.pop(pcm <= 480.0, now) else {
                        break;
                    };
                    if let Some(packet) = packet {
                        let arrival = u64::from_le_bytes(packet[..].try_into().unwrap()) as f64;
                        report.final_delay = ms as f64 + pcm / 48.0 - arrival;
                        report.max_delay = report.max_delay.max(report.final_delay);
                    } else {
                        report.concealments += 1;
                    }
                    pcm += 960.0;
                    started = true;
                    continue;
                }
                let Some(p) = j.poll(now, pcm.round() as usize) else {
                    break;
                };
                pcm += 960.0 / (1.0 + p.correction_ppm / 1e6);
                started = true;
            }
        }
        report.max_pcm = report.max_pcm.max(pcm);
        if started && pcm < 47.0 {
            report.underrun_ms += 1;
        }
        pcm = (pcm - 48.0 * (1.0 + output_ppm / 1e6)).max(0.0);
        let m = j.metrics();
        assert!(m.packets <= MAX_PACKETS);
        assert!((40.0..=120.0).contains(&m.target_ms));
        assert!(m.correction_ppm.abs() <= 5000.0);
        if !old {
            report.max_delay = report.max_delay.max(m.current_ms);
        }
        report.max_target = if old {
            60.0
        } else {
            report.max_target.max(m.target_ms)
        };
        report.max_correction = report.max_correction.max(m.correction_ppm.abs());
    }
    let m = j.metrics();
    if !old {
        report.final_delay = m.current_ms;
    }
    report.final_correction = m.correction_ppm;
    if !old {
        report.concealments = m.concealments;
    }
    report.rebufferings = m.rebufferings;
    report
}

#[test]
fn deterministic_comparison_with_audited_sequence_only_playout() {
    fn bursts(i: usize) -> Option<usize> {
        if (500..506).contains(&i) || i == 900 {
            None
        } else if (200..400).contains(&i) && i % 10 < 3 {
            Some(80)
        } else {
            Some(0)
        }
    }
    for (name, seconds, drift, network) in [
        ("LAN", 60, 0.0, (|_| Some(0)) as fn(usize) -> Option<usize>),
        ("burst/loss", 180, 0.0, bursts),
        ("fast sender", 1800, 400.0, |_| Some(0)),
        ("slow sender", 1800, -400.0, |_| Some(0)),
    ] {
        let old = run_trace(seconds, drift, 0.0, network, true);
        let new = run_trace(seconds, drift, 0.0, network, false);
        println!("comparison {name}\nold {old:?}\nnew {new:?}");
        assert!(new.max_pcm < old.max_pcm);
        if name == "burst/loss" {
            assert!(new.concealments < old.concealments);
        }
        if drift > 0.0 {
            assert!(new.max_delay < old.max_delay / 2.0);
        }
        if drift < 0.0 {
            assert!(new.concealments < old.concealments);
        }
    }
}

#[test]
fn stable_lan_and_independent_seeded_jitter_paths() {
    let lan = trace(60, 0.0, 0.0, |_| Some(0));
    let jitter = trace(60, 0.0, 0.0, |i| {
        Some((i.wrapping_mul(1664525).wrapping_add(1013904223) >> 8) % 24)
    });
    println!("LAN {lan:?}\nseeded jitter {jitter:?}");
    assert_eq!(lan.concealments, 0);
    assert_eq!(lan.underrun_ms, 0);
    assert_eq!(lan.max_target, 40.0);
    assert!(jitter.max_target > lan.max_target + 5.0);
    assert_eq!(jitter.concealments, 0);
    assert_eq!(jitter.underrun_ms, 0);
    assert!(jitter.max_pcm <= 1690.0);
}

#[test]
fn long_calls_control_sender_and_output_clock_drift_without_frame_deletion() {
    for (sender, output) in [(400.0, 0.0), (-400.0, 0.0), (0.0, 400.0), (200.0, -200.0)] {
        let report = trace(1800, sender, output, |_| Some(0));
        println!("clock sender={sender} output={output}: {report:?}");
        assert_eq!(report.concealments, 0);
        assert_eq!(report.rebufferings, 0);
        assert_eq!(report.underrun_ms, 0);
        assert!((report.final_correction - (sender - output)).abs() < 100.0);
        assert!((report.final_delay - 40.0).abs() < 5.0);
        assert!(report.max_delay < 60.0);
        assert!(report.max_pcm <= 1690.0);
    }
}

#[test]
fn burst_jitter_and_loss_remain_bounded_and_stable_paths_recover() {
    let report = trace(180, 0.0, 0.0, |i| {
        if (500..506).contains(&i) || i == 900 {
            None
        } else if (200..400).contains(&i) && i % 10 < 3 {
            Some(80)
        } else {
            Some(0)
        }
    });
    println!("bursts/loss {report:?}");
    assert!(report.max_target > 60.0);
    assert!(report.concealments > 0);
    assert!(report.rebufferings > 0);
    assert!(report.final_delay < 55.0);
    assert!(report.max_delay < 250.0);
    assert!(report.max_pcm <= 1690.0);
}
