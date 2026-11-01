use super::*;
use crate::audio::{
    Remote, codec, fixture,
    mixer::{self, FRAME, RATE},
    transmit,
};
use bytes::Bytes;
use std::sync::{Arc, atomic::Ordering};

/// One report per second for a sender emitting 50 packets per second.
fn report(start: Instant, second: u64, lost: u32, jitter_ms: u32) -> Sample {
    Sample {
        source: 1,
        generation: 0,
        reports: second + 1,
        sequence: second as u32 * 50,
        lost,
        jitter: jitter_ms * 48,
        at: start + Duration::from_secs(second),
    }
}

/// Feeds one-second report windows with a per-window loss count and jitter.
struct Feed {
    c: Controller,
    start: Instant,
    lost: u32,
}
impl Feed {
    fn new() -> Self {
        Self {
            c: Controller::default(),
            start: Instant::now(),
            lost: 0,
        }
    }
    fn run(
        &mut self,
        from: u64,
        seconds: u64,
        lost_per_second: u32,
        jitter_ms: u32,
        rtt_ms: Option<f64>,
    ) -> (NetworkLevel, bool) {
        let mut result = (NetworkLevel::Good, false);
        for second in from..from + seconds {
            self.lost += lost_per_second;
            let s = report(self.start, second, self.lost, jitter_ms);
            result = self.c.update(true, Some(s), rtt_ms, s.at);
        }
        result
    }
}

#[test]
fn classification_separates_congestion_from_random_loss() {
    let w = |loss_percent, jitter_ms, queueing_ms| Window {
        loss_percent,
        jitter_ms,
        queueing_ms,
    };
    assert_eq!(classify(w(0.0, 5.0, Some(10.0))), NetworkLevel::Good);
    // Random loss alone is protection work, not a reason to lower bitrate.
    assert_eq!(classify(w(8.0, 5.0, Some(10.0))), NetworkLevel::Good);
    assert_eq!(classify(w(0.0, 30.0, None)), NetworkLevel::Fair);
    assert_eq!(classify(w(0.0, 5.0, Some(80.0))), NetworkLevel::Fair);
    assert_eq!(classify(w(12.0, 5.0, None)), NetworkLevel::Poor);
    assert_eq!(classify(w(0.0, 55.0, None)), NetworkLevel::Poor);
    assert_eq!(classify(w(0.0, 5.0, Some(160.0))), NetworkLevel::Poor);
    assert_eq!(classify(w(25.0, 0.0, None)), NetworkLevel::Severe);
    assert_eq!(classify(w(0.0, 5.0, Some(350.0))), NetworkLevel::Severe);
    assert_eq!(bitrate(NetworkLevel::Good), 32_000);
    assert_eq!(bitrate(NetworkLevel::Severe), 12_000);
}

#[test]
fn congestion_degrades_after_two_windows_and_recovers_one_step_per_ten_seconds() {
    let mut f = Feed::new();
    let start = f.start;
    // Baseline report, then a single bad window is not enough.
    f.run(0, 1, 0, 2, None);
    assert_eq!(f.run(1, 1, 0, 60, None).0, NetworkLevel::Good);
    // A second consecutive bad window steps to the milder classification.
    assert_eq!(f.run(2, 1, 0, 90, None).0, NetworkLevel::Poor);
    // Clean windows: one step per ten seconds, never straight back to Good.
    assert_eq!(f.run(3, 9, 0, 2, None).0, NetworkLevel::Poor);
    assert_eq!(f.run(12, 2, 0, 2, None).0, NetworkLevel::Fair);
    assert_eq!(f.run(14, 11, 0, 2, None).0, NetworkLevel::Good);
    // A bad window during recovery restarts the calm timer.
    f.run(25, 2, 0, 30, None);
    assert_eq!(f.c.diagnostics(start).level, NetworkLevel::Fair);
    f.run(27, 5, 0, 2, None);
    f.run(32, 1, 0, 30, None);
    assert_eq!(f.run(33, 9, 0, 2, None).0, NetworkLevel::Fair);
}

#[test]
fn queueing_delay_uses_recent_minimum_rtt() {
    let mut f = Feed::new();
    let start = f.start;
    f.run(0, 5, 0, 2, Some(40.0));
    assert_eq!(f.c.diagnostics(start).queueing_delay_ms, Some(0.0));
    let (level, _) = f.run(5, 2, 0, 2, Some(240.0));
    assert_eq!(level, NetworkLevel::Poor);
    let q = f.c.diagnostics(start).queueing_delay_ms.unwrap();
    assert!((150.0..=200.0).contains(&q), "queueing {q}");
}

#[test]
fn per_frame_updates_do_not_erode_the_rtt_baseline() {
    // Production calls the controller every 20 ms with the latest (cached)
    // report and RTT; only accepted windows may age the baseline.
    let start = Instant::now();
    let mut c = Controller::default();
    let mut level = NetworkLevel::Good;
    for frame in 0..(8 * 50) {
        let second = frame / 50;
        let at = start + Duration::from_millis(frame * 20);
        let rtt = if second < 4 { 40.0 } else { 240.0 };
        level = c
            .update(true, Some(report(start, second, 0, 2)), Some(rtt), at)
            .0;
    }
    assert_eq!(level, NetworkLevel::Poor);
    let q = c.diagnostics(start).queueing_delay_ms.unwrap();
    assert!(q >= 150.0, "queueing {q}");
}

#[test]
fn random_loss_requests_dred_and_congestion_withdraws_it() {
    let mut f = Feed::new();
    let start = f.start;
    f.run(0, 1, 0, 2, None);
    // 4/50 = 8% loss with low jitter: keep the bitrate, request DRED.
    assert_eq!(f.run(1, 1, 4, 2, None), (NetworkLevel::Good, false));
    assert_eq!(f.run(2, 1, 4, 2, None), (NetworkLevel::Good, true));
    // Moderate loss holds DRED; congestion removes it immediately.
    assert!(f.run(3, 3, 1, 2, None).1);
    assert!(!f.run(6, 1, 4, 30, None).1);
    // Back to Good after recovery; then clean windows clear DRED after 20 s.
    f.run(7, 14, 0, 2, None);
    assert_eq!(f.c.diagnostics(start).level, NetworkLevel::Good);
    assert!(f.run(21, 2, 4, 2, None).1);
    assert!(f.run(23, 19, 0, 2, None).1);
    assert!(!f.run(42, 2, 0, 2, None).1);
}

#[test]
fn silence_holds_then_forgets_and_source_or_toggle_resets() {
    let mut f = Feed::new();
    let start = f.start;
    f.run(0, 3, 0, 90, None);
    assert_eq!(f.c.diagnostics(start).level, NetworkLevel::Severe);
    // No new report (silence/DTX): hold the measured level for a minute.
    let last = report(start, 2, f.lost, 90);
    let held =
        f.c.update(true, Some(last), None, start + Duration::from_secs(40));
    assert_eq!(held.0, NetworkLevel::Severe);
    let forgotten =
        f.c.update(true, Some(last), None, start + Duration::from_secs(63));
    assert_eq!(forgotten, (NetworkLevel::Good, false));
    f.run(100, 3, 0, 90, None);
    let replacement = Sample {
        source: 2,
        ..report(start, 103, f.lost, 90)
    };
    assert_eq!(
        f.c.update(true, Some(replacement), None, replacement.at).0,
        NetworkLevel::Good
    );
    f.run(104, 3, 0, 90, None);
    assert_eq!(
        f.c.update(false, None, None, start).0,
        NetworkLevel::Good,
        "disabled reports the default"
    );
    assert!(!f.c.diagnostics(start).enabled);
}

fn encoder_at(level_jitter_ms: u32, loss_per_second: u32) -> (transmit::Encoder, Instant) {
    let start = Instant::now();
    let mut encoder = transmit::Encoder::new(true).unwrap();
    let mut lost = 0;
    for second in 0..4 {
        lost += loss_per_second;
        let s = report(start, second, lost, level_jitter_ms);
        encoder.adapt_network(true, Some(s), None, s.at).unwrap();
        encoder.adapt(true, 5, Some(s), s.at).unwrap();
    }
    (encoder, start)
}

fn padded(packet: &[u8]) -> bool {
    packet.len() > 2 && packet[0] & 3 == 3 && packet[1] & 0x40 != 0
}

#[test]
fn encoder_bitrate_follows_level_and_low_rates_use_silk() {
    for (jitter, expected) in [(2, 32_000), (30, 24_000), (60, 16_000), (90, 12_000)] {
        let (mut encoder, _) = encoder_at(jitter, 0);
        assert_eq!(
            encoder.codec.get_bitrate().unwrap(),
            opus::Bitrate::Bits(expected)
        );
        assert_eq!(encoder.bitrate(), expected);
        let mut bytes = 0;
        let mut silk = 0;
        let frames = fixture::speech();
        let frames = &frames.as_chunks::<FRAME>().0[..200];
        for pcm in frames {
            let mut output = [0; 1500];
            let (n, _, _) = encoder.encode(pcm, true, &mut output).unwrap();
            bytes += n;
            // TOC configurations 0-11 are SILK-only.
            silk += usize::from(output[0] >> 3 < 12);
        }
        let kbps = bytes as f64 * 8.0 / (frames.len() as f64 * 0.02) / 1000.0;
        assert!(
            kbps <= f64::from(expected) / 1000.0 * 1.15,
            "{expected}: {kbps:.1} kbit/s"
        );
        if expected == 12_000 {
            assert!(silk > frames.len() * 9 / 10, "12 kbit/s is SILK wideband");
        }
    }
    // Manual DRED keeps its budget only while uncongested.
    let (mut encoder, _) = encoder_at(90, 0);
    encoder.set_deep_redundancy(true).unwrap();
    assert_eq!(encoder.bitrate(), 12_000);
    let (mut encoder, _) = encoder_at(2, 0);
    encoder.set_deep_redundancy(true).unwrap();
    assert_eq!(encoder.bitrate(), crate::audio::dred::BITRATE);
}

#[test]
fn automatic_dred_starts_only_after_silence_and_stops_immediately() {
    let (mut encoder, start) = encoder_at(2, 4);
    assert!(encoder.network_diagnostics(start).automatic_dred);
    let speech = fixture::speech();
    let frames = speech.as_chunks::<FRAME>().0;
    let mut output = [0; 1500];
    // Mid-talkspurt: requested but not latched, so no stale latents are sent.
    for pcm in &frames[..40] {
        let (n, _, _) = encoder.encode(pcm, true, &mut output).unwrap();
        assert!(!padded(&output[..n]));
    }
    assert_eq!(encoder.bitrate(), 32_000);
    // The production worker resets the encoder at the talkspurt end.
    encoder.reset().unwrap();
    encoder.encode(&[0.0; FRAME], false, &mut output).unwrap();
    assert_eq!(encoder.bitrate(), crate::audio::dred::BITRATE);
    assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), 20);
    let carrying = frames[40..140]
        .iter()
        .filter(|pcm| {
            let (n, _, _) = encoder.encode(pcm, true, &mut output).unwrap();
            assert!(n <= 1100);
            padded(&output[..n])
        })
        .count();
    assert!(carrying > 80, "speech carries DRED: {carrying}");
    // Congestion withdraws DRED without waiting for silence.
    let mut lost = 16;
    for second in 4..7 {
        lost += 4;
        let s = report(start, second, lost, 90);
        encoder.adapt_network(true, Some(s), None, s.at).unwrap();
    }
    assert_eq!(encoder.bitrate(), 12_000);
    let (n, _, _) = encoder.encode(&frames[150], true, &mut output).unwrap();
    assert!(!padded(&output[..n]));
    // Disabling adaptation restores the fixed policy.
    encoder
        .adapt_network(false, None, None, start + Duration::from_secs(8))
        .unwrap();
    assert_eq!(encoder.bitrate(), 32_000);
}

#[test]
fn receive_policy_needs_loss_and_dred_carrying_packets_and_holds_through_silence() {
    let start = Instant::now();
    let at = |s: u64| start + Duration::from_secs(s);
    let mut r = Receive::default();
    let dred = [0x7b_u8, 0x41, 0x05, 0x00];
    let plain = [0x78_u8, 0x01, 0x02];
    assert!(!r.update(at(0), 0, 0));
    // Loss without DRED donors only adds latency: stay off.
    assert!(!r.update(at(1), 46, 4));
    r.observe_packet(&plain, at(1));
    assert!(!r.update(at(2), 92, 8));
    r.observe_packet(&dred, at(2));
    assert!(!r.update(at(3), 138, 12));
    assert!(r.update(at(4), 184, 16));
    // Silence: too few packets to judge, hold the state.
    assert!(r.update(at(5), 186, 16));
    assert!(r.update(at(14), 188, 16));
    // Active media that stopped carrying DRED for ten seconds: off.
    assert!(!r.update(at(15), 238, 16));
    // Clean DRED-carrying media clears after thirty seconds.
    let mut r = Receive::default();
    r.update(at(0), 0, 0);
    for s in 1..=3 {
        r.observe_packet(&dred, at(s));
        r.update(at(s), s * 46, s * 4);
    }
    assert!(r.wanted);
    let mut played = 138;
    for s in 4..=33 {
        r.observe_packet(&dred, at(s));
        played += 50;
        r.update(at(s), played, 12);
    }
    assert!(r.wanted);
    r.observe_packet(&dred, at(35));
    assert!(!r.update(at(35), played + 100, 12));
}

fn remote(at: Instant, source_epoch: u32) -> Remote {
    Remote {
        source_epoch,
        privacy_epoch: source_epoch,
        since: at,
        deafened: false,
        decode_stats: Default::default(),
        activity: Default::default(),
        id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        label: "network fixture".into(),
        jitter: Default::default(),
        decoder: opus::Decoder::new(RATE, opus::Channels::Mono).unwrap(),
        rate: Default::default(),
        adaptation: false,
        receive: Default::default(),
    }
}

fn dred_packets() -> Vec<Bytes> {
    let mut encoder = transmit::Encoder::new(true).unwrap();
    encoder.set_deep_redundancy(true).unwrap();
    speech_packets(&mut encoder)
}
fn speech_packets(encoder: &mut transmit::Encoder) -> Vec<Bytes> {
    fixture::speech()
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|pcm| {
            let mut output = [0; 1500];
            let (n, _, _) = encoder.encode(pcm, true, &mut output).unwrap();
            Bytes::copy_from_slice(&output[..n])
        })
        .collect()
}

#[test]
fn remote_uses_osce_for_current_sources_and_enables_dred_under_measured_loss() {
    let at = Instant::now() + Duration::from_secs(1);
    let (mut writers, mut mixer) = mixer::mixer(Arc::new(mixer::Controls::default()));
    let writer = &mut writers[0];
    writer.control.active.store(true, Ordering::Release);
    let mut legacy = remote(at, 0);
    legacy.configure(writer, false, true, at).unwrap();
    assert_eq!(legacy.decoder.get_complexity().unwrap(), 0);
    assert!(!legacy.decoder.get_osce_bwe().unwrap());

    let encoded = dred_packets();
    let mut speaker = remote(at, 3);
    speaker.configure(writer, false, true, at).unwrap();
    assert_eq!(
        speaker.decoder.get_complexity().unwrap(),
        DECODER_COMPLEXITY
    );
    assert!(speaker.decoder.get_osce_bwe().unwrap());
    assert_eq!(speaker.jitter.metrics().target_ms, 40.0);
    let mut enabled_at = None;
    for ms in 0..(encoded.len() * 20 - 100) {
        let now = at + Duration::from_millis(ms as u64);
        if ms.is_multiple_of(20) {
            let n = ms / 20;
            // About 8% isolated loss.
            if n % 12 != 5 {
                speaker.push(n as u16, (n * FRAME) as u32, encoded[n].clone(), now, now);
            }
        }
        if ms.is_multiple_of(5) {
            speaker.adapt_receive(writer, false, now).unwrap();
            if speaker.jitter.deep_redundancy {
                enabled_at.get_or_insert(ms);
            }
            speaker.refill(writer, now);
        }
        let mut pcm = [0.0_f32; 48];
        mixer.render(&mut pcm, 1);
    }
    let enabled_at = enabled_at.expect("measured loss enables DRED receive recovery");
    assert!(
        (2000..=4500).contains(&enabled_at),
        "enabled at {enabled_at} ms"
    );
    assert_eq!(speaker.jitter.metrics().target_ms, 80.0);
    assert_eq!(
        speaker.decoder.get_complexity().unwrap(),
        DECODER_COMPLEXITY
    );
    assert!(speaker.decode_stats.dred_output_frames > 0);
    // The sender stops DRED while speech continues: shutdown waits for a quiet
    // stream, then applies (it must not be forgotten during silence).
    let plain = {
        let mut encoder = transmit::Encoder::new(true).unwrap();
        speech_packets(&mut encoder)
    };
    let base = encoded.len() * 20 - 100;
    let mut deferred = false;
    for ms in 0..15_000usize {
        let now = at + Duration::from_millis((base + ms) as u64);
        if ms < 12_000 && ms.is_multiple_of(20) {
            let n = (base + ms) / 20;
            speaker.push(
                n as u16,
                (n * FRAME) as u32,
                plain[n % plain.len()].clone(),
                now,
                now,
            );
        }
        if ms.is_multiple_of(5) {
            speaker.adapt_receive(writer, false, now).unwrap();
            if ms < 12_000 {
                deferred |= !speaker.receive.wanted && speaker.jitter.deep_redundancy;
            }
            speaker.refill(writer, now);
        }
        let mut pcm = [0.0_f32; 48];
        mixer.render(&mut pcm, 1);
    }
    assert!(deferred, "shutdown requested during speech is deferred");
    assert!(!speaker.jitter.deep_redundancy, "applied once quiet");
    assert_eq!(speaker.jitter.metrics().target_ms, 40.0);
    // Disabling adaptation restores the plain decoder and buffer.
    speaker.configure(writer, false, false, at).unwrap();
    assert_eq!(speaker.decoder.get_complexity().unwrap(), 0);
    assert!(!speaker.jitter.deep_redundancy);
    assert!(!speaker.decoder.get_osce_bwe().unwrap());
}

/// Energy above 8 kHz with a naive DFT over a few frames (test only).
fn high_band_energy(pcm: &[f32]) -> f64 {
    let n = pcm.len();
    let mut total = 0.0;
    for k in (n * 8_000 / RATE as usize)..(n * 20_000 / RATE as usize) {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, x) in pcm.iter().enumerate() {
            let phase = -2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
            re += f64::from(*x) * phase.cos();
            im += f64::from(*x) * phase.sin();
        }
        total += re * re + im * im;
    }
    total
}

#[test]
fn osce_extends_low_bitrate_silk_and_survives_full_reset() {
    let speech = fixture::speech();
    let frames = speech.as_chunks::<FRAME>().0;
    let mut encoder = codec::microphone_encoder().unwrap();
    encoder.set_bitrate(opus::Bitrate::Bits(12_000)).unwrap();
    let packets: Vec<Vec<u8>> = frames
        .iter()
        .map(|pcm| {
            let mut output = [0; 1500];
            let n = encoder.encode_float(pcm, &mut output).unwrap();
            output[..n].to_vec()
        })
        .collect();
    let decode = |complexity: i32, bwe: bool| {
        let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        decoder.set_complexity(complexity).unwrap();
        decoder.set_osce_bwe(bwe).unwrap();
        let mut out = Vec::with_capacity(packets.len() * FRAME);
        let timer = Instant::now();
        for packet in &packets {
            let mut pcm = [0.0; FRAME];
            assert_eq!(
                decoder.decode_float(packet, &mut pcm, false).unwrap(),
                FRAME
            );
            out.extend_from_slice(&pcm);
        }
        let per_frame = timer.elapsed().as_secs_f64() * 1e6 / packets.len() as f64;
        (out, per_frame)
    };
    let (plain, plain_us) = decode(0, false);
    let (osce, osce_us) = decode(DECODER_COMPLEXITY, true);
    // Speech-dense region; SILK wideband carries nothing above 8 kHz.
    let region = 100 * FRAME..104 * FRAME;
    let plain_high = high_band_energy(&plain[region.clone()]);
    let osce_high = high_band_energy(&osce[region]);
    eprintln!(
        "12 kbit/s SILK: plain {plain_us:.1} us/frame, OSCE {osce_us:.1} us/frame; >8 kHz energy {plain_high:.3e} vs {osce_high:.3e}; informational timing"
    );
    // Measured ratios: about 17x on Windows x64, 8.6x on macOS ARM64, where
    // the plain decoder leaks more energy above 8 kHz.
    assert!(
        osce_high > plain_high * 4.0,
        "bandwidth extension must add fullband content"
    );
    let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    decoder.set_complexity(DECODER_COMPLEXITY).unwrap();
    decoder.set_osce_bwe(true).unwrap();
    let mut pcm = [0.0; FRAME];
    for packet in &packets[..30] {
        decoder.decode_float(packet, &mut pcm, false).unwrap();
    }
    decoder.reset_full_state().unwrap();
    assert_eq!(decoder.get_complexity().unwrap(), DECODER_COMPLEXITY);
    assert!(decoder.get_osce_bwe().unwrap());
    let mut fresh = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
    fresh.set_complexity(DECODER_COMPLEXITY).unwrap();
    fresh.set_osce_bwe(true).unwrap();
    let mut expected = [0.0; FRAME];
    for packet in [&[][..], &packets[70], &[], &packets[71]] {
        decoder.decode_float(packet, &mut pcm, false).unwrap();
        fresh.decode_float(packet, &mut expected, false).unwrap();
        assert!(pcm == expected, "full reset must clear OSCE/neural state");
    }
}
