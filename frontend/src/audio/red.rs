//! Native RED sender history; bounded encoded bytes, never PCM or IPC.
#[path = "red_boundary.rs"]
pub mod boundary;
use super::outgoing::EncodedFrame;
use std::time::Instant;
use thiscord_shared::rtp_red::{self as wire, Block};

/// Epoch-zero legacy membership has no ordered source identity. Request a fresh
/// connection when it replaces a protected slot. The supervisor retries with
/// backoff; never clear a live session's tombstones to accept the downgrade.
#[derive(Default)]
pub struct RosterCompatibility {
    protected: [bool; thiscord_shared::voice::ROOM_CAPACITY],
}
impl RosterCompatibility {
    pub fn observe(
        &mut self,
        members: &[thiscord_shared::voice::Participant],
    ) -> Result<(), super::reconnect::Failure> {
        for member in members {
            let Some(protected) = self.protected.get_mut(member.slot) else {
                continue;
            };
            if member.microphone_source_epoch == 0 && *protected {
                return Err(super::reconnect::Failure::temporary(
                    "Refreshing voice compatibility for a participant",
                )
                .with_reason("legacy_source_replacement"));
            }
            *protected |= member.microphone_source_epoch != 0;
        }
        Ok(())
    }
}

impl super::Remote {
    pub(super) fn push_transport(
        &mut self,
        pt: u8,
        sequence: u16,
        timestamp: u32,
        payload: bytes::Bytes,
        arrived: Instant,
        now: Instant,
    ) -> bool {
        if pt == wire::OPUS_PAYLOAD_TYPE {
            return self.push(sequence, timestamp, payload, arrived, now);
        }
        if pt != wire::PAYLOAD_TYPE {
            return false;
        }
        let Some(packet) = wire::parse(&payload) else {
            return false;
        };
        if !self.push(
            sequence,
            timestamp,
            bytes::Bytes::copy_from_slice(packet.primary),
            arrived,
            now,
        ) {
            return false;
        }
        for block in packet.history {
            if opus::packet::get_nb_samples(block.payload, 48_000).is_ok_and(|n| n == 960) {
                self.jitter.redundant(
                    timestamp.wrapping_sub(u32::from(block.offset)),
                    arrived,
                    bytes::Bytes::copy_from_slice(block.payload),
                );
            }
        }
        true
    }
}

#[derive(Default)]
pub struct Sender {
    history: Vec<EncodedFrame>,
}
impl Sender {
    pub fn clear(&mut self) {
        self.history.clear();
    }
    pub fn deadline(&self, primary: &EncodedFrame) -> Instant {
        self.history
            .iter()
            .map(EncodedFrame::deadline)
            .fold(primary.deadline(), Instant::min)
    }
    pub fn within_budget(&mut self, frame: &EncodedFrame) -> bool {
        if frame.payload.is_empty() || frame.payload.len() > wire::MAX_PAYLOAD {
            self.clear();
            false
        } else {
            true
        }
    }

    pub fn packet(
        &mut self,
        frame: &EncodedFrame,
        enabled: bool,
        now: Instant,
    ) -> (u8, bytes::Bytes) {
        self.history.retain(|old| {
            enabled
                && old.generation == frame.generation
                && old.connection.accepts(&frame.connection)
                && old.delivery.active()
                && frame.delivery.accepts(&old.delivery)
                && now < old.deadline()
                && frame
                    .media_position
                    .checked_sub(old.media_position)
                    .is_some_and(|delta| delta == 960 || delta == 1920)
        });
        if !enabled || frame.marker || !wire::repeatable(&frame.payload) {
            self.history.clear();
        }
        if !enabled {
            return (wire::OPUS_PAYLOAD_TYPE, frame.payload.clone());
        }
        let history: Vec<_> = self
            .history
            .iter()
            .map(|old| Block {
                offset: (frame.media_position - old.media_position) as u16,
                payload: &old.payload,
            })
            .collect();
        match wire::encode(&frame.payload, &history) {
            Some(payload) => (wire::PAYLOAD_TYPE, payload.into()),
            None => (wire::OPUS_PAYLOAD_TYPE, frame.payload.clone()),
        }
    }
    /// Retain only frames actually admitted to the transport, with their original
    /// capture deadline, privacy generation and delivery lease.
    pub fn admitted(&mut self, frame: EncodedFrame, enabled: bool) {
        if enabled && wire::repeatable(&frame.payload) {
            if self.history.len() == wire::DEPTH {
                self.history.remove(0);
            }
            self.history.push(frame);
        } else {
            self.history.clear();
        }
    }
}

pub fn register(engine: &mut webrtc::peer_connection::MediaEngine) -> Result<(), String> {
    use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind};
    engine
        .register_codec(
            RTCRtpCodecParameters {
                rtp_codec: RTCRtpCodec {
                    mime_type: wire::MIME.into(),
                    clock_rate: 48_000,
                    channels: 2,
                    sdp_fmtp_line: wire::FMTP.into(),
                    ..Default::default()
                },
                payload_type: wire::PAYLOAD_TYPE,
            },
            RtpCodecKind::Audio,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Remote, connection::Connection, jitter::Playout};
    use std::time::Duration;

    fn frame(at: Instant, position: u64, payload: &[u8], connection: &Connection) -> EncodedFrame {
        EncodedFrame {
            accounted: true,
            packet_redundancy: true,
            payload: bytes::Bytes::copy_from_slice(payload),
            media_position: position,
            marker: false,
            oldest_capture: at,
            generation: 0,
            connection: connection.clone(),
            delivery: connection.clone(),
            activation_delay: Duration::ZERO,
        }
    }
    fn remote(now: Instant) -> Remote {
        Remote {
            source_epoch: 0,
            privacy_epoch: 0,
            since: now,
            deafened: false,
            decode_stats: Default::default(),
            activity: Default::default(),
            id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            label: "RED test".into(),
            jitter: Default::default(),
            rate: Default::default(),
            decoder: opus::Decoder::new(48_000, opus::Channels::Mono).unwrap(),
        }
    }
    fn opus_packets() -> Vec<Vec<u8>> {
        let mut encoder = crate::audio::codec::microphone_encoder().unwrap();
        encoder.set_vbr(false).unwrap();
        (0..10)
            .map(|frame| {
                let pcm = std::array::from_fn::<_, 960, _>(|i| {
                    ((i + frame * 960) as f32 * 0.07).sin() * 0.1
                });
                let mut output = [0; 1100];
                let n = encoder.encode_float(&pcm, &mut output).unwrap();
                assert_eq!(n, 80);
                output[..n].to_vec()
            })
            .collect()
    }

    #[test]
    fn adaptive_fec_red_counts_outer_packets_and_keeps_feedback_across_privacy() {
        use crate::audio::transmit::Encoder;
        use rtc::rtcp::reception_report::ReceptionReport;
        use thiscord_shared::voice::MediaKind;

        let start = Instant::now();
        let connection = Connection::default();
        let mut encoder = Encoder::new(false).unwrap();
        encoder.codec.set_vbr(false).unwrap();
        let mut sender = Sender::default();
        let mut privacy = 1;
        let mut copies = 0;
        for sequence in 0..=100u16 {
            let now = start + Duration::from_millis(u64::from(sequence) * 20);
            if sequence == 51 {
                privacy += 1;
                sender.clear();
                encoder.reset().unwrap();
                assert!(connection.uplink_sample(0).is_some());
            }
            encoder
                .adapt(true, 5, connection.uplink_sample(0), now)
                .unwrap();
            let pcm = std::array::from_fn(|i| (i as f32 * 0.07).sin() * 0.1);
            let mut bytes = [0; 1100];
            let (len, omitted, _) = encoder.encode(&pcm, false, &mut bytes).unwrap();
            assert!(!omitted);
            let mut current = frame(now, u64::from(sequence) * 960, &bytes[..len], &connection);
            current.generation = privacy;
            let enabled = !(60..70).contains(&sequence);
            let (pt, payload) = sender.packet(&current, enabled, now);
            if enabled {
                let parsed = wire::parse(&payload).unwrap();
                copies += parsed.history.len();
                if sequence == 51 || sequence == 70 {
                    assert!(parsed.history.is_empty());
                }
            } else {
                assert_eq!(pt, wire::OPUS_PAYLOAD_TYPE);
            }
            // Production records only a successfully written outer packet.
            connection.uplink_sent(0, sequence);
            sender.admitted(current, enabled);
            if sequence.is_multiple_of(50) {
                connection.observe_uplink(
                    &ReceptionReport {
                        ssrc: MediaKind::Microphone.publisher_ssrc(),
                        last_sequence_number: u32::from(sequence),
                        total_lost: u32::from(sequence / 5),
                        ..Default::default()
                    },
                    now,
                );
                encoder
                    .adapt(true, 5, connection.uplink_sample(0), now)
                    .unwrap();
            }
        }
        assert!(copies > 100);
        assert!(encoder.codec.get_packet_loss_perc().unwrap() > 5);
        // No outer packet 150 was emitted: copies cannot authorize that report.
        let later = start + Duration::from_secs(3);
        connection.observe_uplink(
            &ReceptionReport {
                ssrc: MediaKind::Microphone.publisher_ssrc(),
                last_sequence_number: 150,
                total_lost: 30,
                ..Default::default()
            },
            later,
        );
        encoder.reset().unwrap();
        encoder
            .adapt(true, 5, connection.uplink_sample(0), later)
            .unwrap();
        assert_eq!(
            encoder.adaptive.diagnostics(later).feedback_age_ms,
            Some(1000)
        );
        assert!(encoder.codec.get_packet_loss_perc().unwrap() > 5);
        assert!(Connection::default().uplink_sample(0).is_none());
        connection.close();
        encoder
            .adapt(true, 5, connection.uplink_sample(0), later)
            .unwrap();
        assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), 5);
    }

    #[test]
    fn legacy_replacement_requests_recovery_without_forgetting_tombstones() {
        let legacy: thiscord_shared::voice::Participant =
            serde_json::from_value(serde_json::json!({
                "account_id":"00000000-0000-0000-0000-000000000001", "username":"fixture", "slot":0,
                "muted":false,"deafened":false,"can_speak":true
            }))
            .unwrap();
        let mut modern = legacy.clone();
        modern.microphone_source_epoch = 5;
        modern.microphone_epoch = 6;
        let mut roster = RosterCompatibility::default();
        assert!(roster.observe(std::slice::from_ref(&legacy)).is_ok());
        assert!(roster.observe(&[modern]).is_ok());
        assert!(roster.observe(&[]).is_ok());
        let error = roster.observe(std::slice::from_ref(&legacy)).unwrap_err();
        assert!(error.retryable);
        assert_eq!(error.diagnostic_reason(), "legacy_source_replacement");
        assert!(roster.observe(std::slice::from_ref(&legacy)).is_err());
        let mut next_connection = RosterCompatibility::default();
        assert!(next_connection.observe(&[legacy]).is_ok());
    }

    #[tokio::test]
    async fn legacy_join_recovers_once_and_disconnect_cancels_recovery() {
        use crate::audio::reconnect::{Connector, Failure, supervise};
        struct Call {
            attempts: usize,
            waits: usize,
            rejoins: usize,
            cancel_in_backoff: bool,
            stop: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl Connector for Call {
            async fn attempt(&mut self) -> (Failure, bool) {
                self.attempts += 1;
                // As in run(), each full transport attempt owns fresh roster
                // history. Signaling-only recovery must not reset this history.
                let mut roster = RosterCompatibility::default();
                let legacy: thiscord_shared::voice::Participant =
                    serde_json::from_value(serde_json::json!({
                        "account_id":"00000000-0000-0000-0000-000000000001",
                        "username":"fixture", "slot":0,
                        "muted":false,"deafened":false,"can_speak":true
                    }))
                    .unwrap();
                if self.attempts == 1 {
                    let mut modern = legacy.clone();
                    modern.microphone_source_epoch = 5;
                    modern.microphone_epoch = 6;
                    roster.observe(&[modern]).unwrap();
                    roster.observe(&[]).unwrap();
                    return (roster.observe(&[legacy]).unwrap_err(), true);
                }
                // The original incident was followed by repeated leave/join
                // attempts. They must not cause another recovery on the fresh call.
                for _ in 0..5 {
                    roster.observe(std::slice::from_ref(&legacy)).unwrap();
                    roster.observe(&[]).unwrap();
                    self.rejoins += 1;
                }
                self.stop.take().unwrap().send(()).unwrap();
                std::future::pending().await
            }
            fn waiting(&mut self, error: &Failure, delay: Duration) {
                assert_eq!(error.diagnostic_reason(), "legacy_source_replacement");
                assert_eq!(delay, Duration::from_millis(1250));
                self.waits += 1;
                if self.cancel_in_backoff {
                    self.stop.take().unwrap().send(()).unwrap();
                }
            }
        }
        for cancel_in_backoff in [false, true] {
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let mut call = Call {
                attempts: 0,
                waits: 0,
                rejoins: 0,
                cancel_in_backoff,
                stop: Some(stop),
            };
            let result = tokio::time::timeout(
                Duration::from_secs(4),
                supervise(&mut call, stopped, || 250),
            )
            .await
            .unwrap();
            assert!(result.is_none(), "compatibility must not end the call");
            assert_eq!(call.waits, 1);
            assert_eq!(call.attempts, if cancel_in_backoff { 1 } else { 2 });
            assert_eq!(call.rejoins, if cancel_in_backoff { 0 } else { 5 });
        }
    }
    #[test]
    fn isolated_and_burst_losses_recover_exact_packets_without_sequence_invention() {
        let payloads = opus_packets();
        for (enabled, losses, expected) in [
            (false, vec![2, 6], 0),
            (true, vec![2, 6], 2),
            (true, vec![2, 3], 2),
            (true, vec![2, 3, 4], 2),
        ] {
            let now = Instant::now();
            let connection = Connection::default();
            let mut sender = Sender::default();
            let mut receiver = remote(now);
            let mut sent_bytes = 0;
            // Near both wraps: recovery uses timestamp subtraction only.
            let base = u32::MAX - 1920;
            for (i, payload) in payloads.iter().enumerate() {
                let at = now + Duration::from_millis(i as u64 * 20);
                let f = frame(at, u64::from(base) + i as u64 * 960, payload, &connection);
                let (pt, packet) = sender.packet(&f, enabled, at);
                sent_bytes += packet.len();
                sender.admitted(f, enabled);
                if !losses.contains(&i) {
                    assert!(receiver.push_transport(
                        pt,
                        (i as u16).wrapping_sub(3),
                        base.wrapping_add(i as u32 * 960),
                        packet,
                        at,
                        at
                    ));
                }
            }
            let mut recovered = 0;
            for (i, original) in payloads.iter().enumerate() {
                let decision = receiver
                    .jitter
                    .pop(true, now + Duration::from_millis(200))
                    .unwrap();
                if let Playout::Packet(payload) = decision {
                    assert_eq!(payload.as_ref(), original);
                    if losses.contains(&i) {
                        recovered += 1;
                    }
                }
            }
            assert_eq!(recovered, expected);
            // Missing transport packets remain missing in network accounting.
            assert_eq!(receiver.jitter.metrics().sequence_gaps, losses.len() as u64);
            assert_eq!(sent_bytes, if enabled { 2238 } else { 800 });
        }
    }

    #[test]
    fn settings_dtx_padding_size_marker_and_privacy_boundaries_clear_all_history() {
        let now = Instant::now();
        let connection = Connection::default();
        let p = opus_packets().remove(0);
        for boundary in 0..10 {
            let mut sender = Sender::default();
            sender.admitted(frame(now, 0, &p, &connection), true);
            let mut next = frame(now, 960, &p, &connection);
            let enabled = boundary != 0;
            match boundary {
                1 => next.generation = 1,
                2 => next.marker = true,
                3 => next.payload = vec![0x78, 0].into(), // DTX update
                4 => next.payload = vec![3, 0x40, 1].into(), // Opus padding / possible DRED
                5 => next.payload = vec![0x78; 161].into(),
                6 => next.connection = Connection::default(),
                7 => next.delivery = Connection::default(),
                8 => next.media_position = 960 * 10, // DTX omission / capture gap
                9 => next.oldest_capture = now + Duration::from_millis(120),
                _ => (),
            }
            let (pt, payload) = sender.packet(&next, enabled, next.oldest_capture);
            if enabled {
                assert!(wire::parse(&payload).unwrap().history.is_empty());
            } else {
                assert_eq!(pt, wire::OPUS_PAYLOAD_TYPE);
                assert_eq!(payload, next.payload);
            }
            assert!(sender.history.is_empty());
        }
        let mut sender = Sender::default();
        sender.admitted(frame(now, 0, &p, &connection), true);
        let next = frame(now, 960, &vec![0x78; 1100], &connection);
        let (pt, payload) = sender.packet(&next, true, now);
        assert_eq!(pt, wire::OPUS_PAYLOAD_TYPE);
        assert_eq!(payload, next.payload);
        assert!(sender.history.is_empty());
        sender.admitted(next, true);
        assert!(sender.history.is_empty());
        let next = frame(now, 1920, &p, &connection);
        assert!(
            wire::parse(&sender.packet(&next, true, now).1)
                .unwrap()
                .history
                .is_empty()
        );
    }

    #[test]
    fn late_history_reorder_and_reset_never_replay_consumed_audio() {
        let now = Instant::now();
        let p = opus_packets();
        let mut r = remote(now);
        for i in [0, 2, 1] {
            assert!(r.push_transport(
                111,
                i + 1,
                u32::from(i) * 960,
                p[i as usize].clone().into(),
                now,
                now
            ));
        }
        for _ in 0..3 {
            assert!(matches!(r.jitter.pop(true, now), Some(Playout::Packet(_))));
        }
        let packet = wire::encode(
            &p[3],
            &[
                Block {
                    offset: 1920,
                    payload: &p[1],
                },
                Block {
                    offset: 960,
                    payload: &p[2],
                },
            ],
        )
        .unwrap();
        assert!(r.push_transport(112, 4, 2880, packet.clone().into(), now, now));
        assert_eq!(
            r.jitter.pop(true, now),
            Some(Playout::Packet(p[3].clone().into()))
        );
        assert!(!r.push_transport(112, 4, 2880, packet.into(), now, now));
        r.jitter.discard();
        r.since = now + Duration::from_millis(10);
        let packet = wire::encode(
            &p[4],
            &[Block {
                offset: 960,
                payload: &p[3],
            }],
        )
        .unwrap();
        assert!(!r.push_transport(112, 5, 3840, packet.clone().into(), now, now));
        let at = now + Duration::from_millis(20);
        assert!(r.push_transport(112, 5, 3840, packet.into(), at, at));
        assert_eq!(
            r.jitter.pop(true, at + Duration::from_millis(60)),
            Some(Playout::Packet(p[4].clone().into()))
        );
        r.deafened = true;
        assert!(!r.push_transport(111, 6, 4800, p[5].clone().into(), at, at));
    }

    #[test]
    fn steady_state_bandwidth_is_bounded_even_for_small_and_large_payloads() {
        let now = Instant::now();
        let connection = Connection::default();
        for (bytes, expected) in [(3, 18), (80, 249), (160, 489), (161, 162), (1100, 1100)] {
            let mut sender = Sender::default();
            let payload = vec![0x78; bytes];
            let mut size = 0;
            for i in 0..3 {
                let frame = frame(now, i * 960, &payload, &connection);
                let (_, packet) = sender.packet(&frame, true, now);
                size = packet.len();
                sender.admitted(frame, true);
            }
            assert_eq!(size, expected);
            assert!(size <= wire::MAX_PAYLOAD);
        }
    }

    #[test]
    fn rejected_oversize_primary_clears_previous_history_before_next_packet() {
        let now = Instant::now();
        let connection = Connection::default();
        let p = opus_packets().remove(0);
        let mut sender = Sender::default();
        sender.admitted(frame(now, 0, &p, &connection), true);
        let too_big = frame(now, 960, &vec![0x78; wire::MAX_PAYLOAD + 1], &connection);
        assert!(!sender.within_budget(&too_big));
        let next = frame(now, 1920, &p, &connection);
        assert!(
            wire::parse(&sender.packet(&next, true, now).1)
                .unwrap()
                .history
                .is_empty()
        );
    }
}
