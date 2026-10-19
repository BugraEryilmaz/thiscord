use super::*;

#[test]
fn all_eight_policies_keep_single_loss_writer_and_restore_measured_policy() {
    for adaptive in [false, true] {
        for red in [false, true] {
            for dred in [false, true] {
                let settings = thiscord_shared::audio::AudioSettings {
                    adaptive_fec: adaptive,
                    packet_redundancy: red,
                    deep_redundancy: dred,
                    ..Default::default()
                };
                let roundtrip: thiscord_shared::audio::AudioSettings =
                    serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
                assert_eq!(settings, roundtrip);
                let at = Instant::now();
                let mut encoder = transmit::Encoder::new(true).unwrap();
                encoder.set_deep_redundancy(dred).unwrap();
                let minimum = if dred { 20 } else { 5 };
                encoder.adapt(adaptive, 5, None, at).unwrap();
                assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), minimum);
                // Fresh zero-loss feedback cannot lower the DRED budget.
                for second in 0..20 {
                    let report = sample(at, second, second as u32 * 50, 0);
                    encoder.adapt(adaptive, 5, Some(report), report.at).unwrap();
                    assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), minimum);
                }
                // Sustain 30% measured loss, including after a counter reset.
                for second in 20..50 {
                    let report = sample(at, second, second as u32 * 50, (second - 20) as u32 * 15);
                    encoder.adapt(adaptive, 5, Some(report), report.at).unwrap();
                }
                let measured = encoder.adaptive.diagnostics(at).effective_loss_percent;
                if adaptive {
                    assert!(measured >= 28);
                } else {
                    assert_eq!(measured, 5);
                }
                assert_eq!(
                    encoder.codec.get_packet_loss_perc().unwrap(),
                    i32::from(measured).max(minimum)
                );
                for enabled in [true, false, true, false] {
                    encoder.set_deep_redundancy(enabled).unwrap();
                    encoder.reset().unwrap();
                    let floor = if enabled { 20 } else { 5 };
                    assert_eq!(
                        encoder.codec.get_packet_loss_perc().unwrap(),
                        i32::from(measured).max(floor)
                    );
                    assert_eq!(
                        encoder.codec.get_bitrate().unwrap(),
                        opus::Bitrate::Bits(if enabled { 40_000 } else { 32_000 })
                    );
                    let status = encoder.adaptive_diagnostics(at);
                    assert_eq!(status.baseline_loss_percent, floor as u8);
                    assert_eq!(status.effective_loss_percent, measured.max(floor as u8));
                }
                encoder.set_deep_redundancy(dred).unwrap();
                let stale = sample(at, 49, 2450, 435);
                encoder
                    .adapt(adaptive, 5, Some(stale), at + Duration::from_secs(60))
                    .unwrap();
                assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), minimum);
                encoder
                    .adapt(adaptive, 5, None, at + Duration::from_secs(61))
                    .unwrap();
                assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), minimum);
                let replacement = Sample {
                    source: 2,
                    generation: 1,
                    ..sample(at, 62, 1, 0)
                };
                encoder
                    .adapt(adaptive, 5, Some(replacement), replacement.at)
                    .unwrap();
                assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), minimum);
                for _ in 0..300 {
                    encoder
                        .encode(&[0.0; FRAME], false, &mut [0; 1500])
                        .unwrap();
                }
                assert!(encoder.stats.omitted_packets > 250);
            }
        }
    }
}
