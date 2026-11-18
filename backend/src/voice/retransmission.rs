//! The stock responder emits cached packets below application authorization and
//! byte accounting, and expands every NACK bitmap without dedup or budget. Screen
//! and system-audio streams therefore use explicit IDR recovery only; camera NACKs
//! are deduplicated and budgeted per receiving connection before the stock
//! responder sees them. Other SSRCs negotiate no NACK cache and are discarded.
use super::MediaKind;
use rtc::{
    interceptor::{
        Interceptor, NackResponderBuilder, NackResponderInterceptor, Packet, StreamInfo,
        TaggedPacket,
    },
    rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack},
    sansio::Protocol,
    shared::error::Error,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A sequence number retransmitted this recently is not resent again. Shorter
/// than a typical NACK retry interval, so a lost retransmission is still repaired.
const REPEAT_WINDOW: Duration = Duration::from_millis(50);
/// Camera retransmits per second per receiving connection, and the burst. One
/// 1.5 Mbit/s camera is about 160 packets/s, so this covers heavy loss across
/// several viewed cameras while capping a NACK flood near one extra camera stream
/// (about 2 Mbit/s with RTP/SRTP overhead) per connection.
const BUDGET: f64 = 200.0;
/// Direct-mapped dedup slots: constant memory; a collision only forgets a request.
const RECENT: usize = 256;
/// The stock responder's send buffer size (`NackResponderBuilder` default).
const CACHE: u16 = 1024;

#[derive(Clone, Copy)]
struct Sent {
    ssrc: u32,
    seq: u16,
    at: Instant,
}

/// Mirrors which sequence numbers the stock send buffer can answer, so requests
/// for packets it never cached (upstream or SFU drops, which receivers keep
/// re-requesting) cost no budget and cannot crowd out repairable losses.
struct Cached {
    highest: Option<u16>,
    seqs: Box<[Option<u16>; CACHE as usize]>,
}
impl Cached {
    /// Same bookkeeping as the stock `SendBuffer::add`.
    fn add(&mut self, seq: u16) {
        let mask = usize::from(CACHE - 1);
        if let Some(highest) = self.highest {
            let diff = seq.wrapping_sub(highest);
            if diff == 0 {
                return;
            }
            if diff < 1 << 15 {
                for skipped in 1..diff.min(CACHE + 1) {
                    self.seqs[usize::from(highest.wrapping_add(skipped)) & mask] = None;
                }
                self.highest = Some(seq);
            }
        } else {
            self.highest = Some(seq);
        }
        self.seqs[usize::from(seq) & mask] = Some(seq);
    }
    /// Same answer as the stock `SendBuffer::get(seq).is_some()`.
    fn has(&self, seq: u16) -> bool {
        self.highest.is_some_and(|highest| {
            highest.wrapping_sub(seq) < CACHE
                && self.seqs[usize::from(seq) & usize::from(CACHE - 1)] == Some(seq)
        })
    }
}

pub(super) struct Responder {
    inner: NackResponderInterceptor,
    /// Bound camera relay SSRCs only: at most one per room slot.
    cached: HashMap<u32, Cached>,
    recent: Box<[Option<Sent>; RECENT]>,
    tokens: f64,
    refilled: Option<Instant>,
}
impl Default for Responder {
    fn default() -> Self {
        Self {
            inner: NackResponderBuilder::new().with_size(CACHE).build(),
            cached: HashMap::new(),
            recent: Box::new([None; RECENT]),
            tokens: BUDGET,
            refilled: None,
        }
    }
}

fn camera(ssrc: u32) -> bool {
    MediaKind::from_relay_ssrc(ssrc).is_some_and(|(kind, _)| kind == MediaKind::Camera)
}

impl Responder {
    /// Rewrites a camera NACK to the cached sequence numbers admitted by the
    /// repeat window and budget, or `None` to discard it. Once the budget is spent
    /// the remaining pairs are not expanded; the result holds at most `BUDGET`
    /// entries. Only bound camera streams have a cache to answer from.
    fn admit(&mut self, now: Instant, nack: &TransportLayerNack) -> Option<TransportLayerNack> {
        let cached = self.cached.get(&nack.media_ssrc)?;
        if let Some(last) = self.refilled {
            let elapsed = now.saturating_duration_since(last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * BUDGET).min(BUDGET);
        }
        self.refilled = Some(self.refilled.map_or(now, |last| last.max(now)));
        let mut nacks: Vec<NackPair> = Vec::new();
        'pairs: for pair in &nack.nacks {
            let bits = (0..16).filter(|bit| pair.lost_packets & (1 << bit) != 0);
            for seq in std::iter::once(pair.packet_id)
                .chain(bits.map(|bit| pair.packet_id.wrapping_add(bit + 1)))
            {
                if self.tokens < 1.0 {
                    break 'pairs;
                }
                if !cached.has(seq) {
                    continue;
                }
                let index = (usize::from(seq)
                    ^ (nack.media_ssrc.wrapping_mul(0x9E37_79B1) >> 24) as usize)
                    % RECENT;
                let slot = &mut self.recent[index];
                if slot.is_some_and(|sent| {
                    sent.ssrc == nack.media_ssrc
                        && sent.seq == seq
                        && now.saturating_duration_since(sent.at) < REPEAT_WINDOW
                }) {
                    continue;
                }
                *slot = Some(Sent {
                    ssrc: nack.media_ssrc,
                    seq,
                    at: now,
                });
                self.tokens -= 1.0;
                match nacks.last_mut() {
                    Some(last) if (1..=16).contains(&seq.wrapping_sub(last.packet_id)) => {
                        last.lost_packets |= 1 << (seq.wrapping_sub(last.packet_id) - 1);
                    }
                    _ => nacks.push(NackPair {
                        packet_id: seq,
                        lost_packets: 0,
                    }),
                }
            }
        }
        (!nacks.is_empty()).then_some(TransportLayerNack {
            sender_ssrc: nack.sender_ssrc,
            media_ssrc: nack.media_ssrc,
            nacks,
        })
    }
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for Responder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Error> {
        let now = msg.now;
        if let Packet::Rtcp(packets) = &mut msg.message.packet {
            // Filter before the stock responder expands NACK bitmaps or queues
            // retransmits. Allocation only for admitted (budgeted) requests.
            packets.retain_mut(|packet| {
                let Some(nack) = packet.as_any().downcast_ref::<TransportLayerNack>() else {
                    return true;
                };
                match self.admit(now, nack) {
                    Some(admitted) => {
                        *packet = Box::new(admitted);
                        true
                    }
                    None => false,
                }
            });
            if packets.is_empty() {
                return Ok(());
            }
        }
        self.inner.handle_read(msg)
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.inner.poll_read()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Error> {
        if let Packet::Rtp(packet) = &msg.message.packet
            && let Some(cached) = self.cached.get_mut(&packet.header.ssrc)
        {
            cached.add(packet.header.sequence_number);
        }
        self.inner.handle_write(msg)
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.inner.poll_write()
    }
    fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.inner.handle_timeout(now)
    }
    fn poll_timeout(&mut self) -> Option<Instant> {
        self.inner.poll_timeout()
    }
}

impl Interceptor for Responder {
    fn bind_local_stream(&mut self, info: &StreamInfo) {
        // SSRC-based, not codec-based: screen also uses H.264. Never create a
        // screen cache, even if a peer negotiates NACK/RTX for that codec.
        // The mirror follows the stock rule: only streams negotiating plain NACK.
        let nack = info
            .rtcp_feedback
            .iter()
            .any(|fb| fb.typ == "nack" && fb.parameter.is_empty());
        if camera(info.ssrc) && nack {
            self.inner.bind_local_stream(info);
            self.cached.insert(
                info.ssrc,
                Cached {
                    highest: None,
                    seqs: Box::new([None; CACHE as usize]),
                },
            );
        }
    }
    fn unbind_local_stream(&mut self, info: &StreamInfo) {
        self.cached.remove(&info.ssrc);
        self.inner.unbind_local_stream(info);
    }
    fn bind_remote_stream(&mut self, info: &StreamInfo) {
        self.inner.bind_remote_stream(info);
    }
    fn unbind_remote_stream(&mut self, info: &StreamInfo) {
        self.inner.unbind_remote_stream(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::{
        interceptor::{AttributedPacket, RTCPFeedback},
        rtp,
    };

    fn tagged(now: Instant, packet: Packet) -> TaggedPacket {
        TaggedPacket {
            now,
            transport: Default::default(),
            message: AttributedPacket::new(packet),
        }
    }
    fn bound(kind: MediaKind, rtx: bool) -> (Responder, u32) {
        let mut responder = Responder::default();
        let info = StreamInfo {
            ssrc: kind.relay_ssrc(2),
            ssrc_rtx: rtx.then_some(123),
            payload_type_rtx: rtx.then_some(126),
            rtcp_feedback: vec![RTCPFeedback {
                typ: "nack".into(),
                parameter: String::new(),
            }],
            ..Default::default()
        };
        responder.bind_local_stream(&info);
        (responder, info.ssrc)
    }
    /// Sends `count` packets from sequence 0 and checks each passes unchanged.
    fn send(responder: &mut Responder, ssrc: u32, count: u16) {
        for seq in 0..count {
            responder
                .handle_write(tagged(
                    Instant::now(),
                    Packet::Rtp(rtp::Packet {
                        header: rtp::header::Header {
                            version: 2,
                            ssrc,
                            sequence_number: seq,
                            ..Default::default()
                        },
                        payload: vec![0x65; 1200].into(),
                    }),
                ))
                .unwrap();
            assert!(responder.poll_write().is_some()); // Ordinary media always passes.
        }
        assert!(responder.poll_write().is_none());
    }
    /// Delivers one RTCP packet and returns how many retransmits it queued.
    fn nack(responder: &mut Responder, now: Instant, ssrc: u32, nacks: Vec<NackPair>) -> usize {
        responder
            .handle_read(tagged(
                now,
                Packet::Rtcp(vec![Box::new(TransportLayerNack {
                    sender_ssrc: 900,
                    media_ssrc: ssrc,
                    nacks,
                })]),
            ))
            .unwrap();
        while responder.poll_read().is_some() {}
        std::iter::from_fn(|| responder.poll_write()).count()
    }
    fn flood(packet_id: u16) -> Vec<NackPair> {
        vec![
            NackPair {
                packet_id,
                lost_packets: u16::MAX
            };
            32
        ]
    }

    #[test]
    fn only_camera_streams_cache_and_repeated_requests_resend_once_per_window() {
        for kind in MediaKind::ALL {
            for rtx in [false, true] {
                let (mut responder, ssrc) = bound(kind, rtx);
                send(&mut responder, ssrc, 8);
                let start = Instant::now();
                // 32 pairs, each covering 7..=23: only 7 is cached, sent once.
                let expected = usize::from(kind == MediaKind::Camera);
                assert_eq!(nack(&mut responder, start, ssrc, flood(7)), expected);
                for n in 1..100 {
                    let now = start + REPEAT_WINDOW * n / 100;
                    assert_eq!(nack(&mut responder, now, ssrc, flood(7)), 0, "{kind:?}");
                }
                let later = start + REPEAT_WINDOW;
                assert_eq!(nack(&mut responder, later, ssrc, flood(7)), expected);
            }
        }
    }

    #[test]
    fn non_camera_nacks_are_discarded_before_the_stock_responder() {
        for ssrc in [
            MediaKind::ScreenVideo.relay_ssrc(2),
            MediaKind::SystemAudio.relay_ssrc(2),
            MediaKind::Microphone.relay_ssrc(2),
            123_456,
        ] {
            let mut responder = Responder::default();
            responder
                .handle_read(tagged(
                    Instant::now(),
                    Packet::Rtcp(vec![Box::new(TransportLayerNack {
                        sender_ssrc: 900,
                        media_ssrc: ssrc,
                        nacks: flood(7),
                    })]),
                ))
                .unwrap();
            assert!(responder.poll_read().is_none());
            assert!(responder.poll_write().is_none());
        }
    }

    #[test]
    fn ordinary_camera_loss_is_repaired_including_retries() {
        let (mut responder, ssrc) = bound(MediaKind::Camera, false);
        send(&mut responder, ssrc, 64);
        let start = Instant::now();
        // Scattered losses, one encoded with a bitmap: 3, 5, 6, 20, 40.
        let lost = vec![
            NackPair {
                packet_id: 3,
                lost_packets: 0b11,
            },
            NackPair {
                packet_id: 20,
                lost_packets: 0,
            },
            NackPair {
                packet_id: 40,
                lost_packets: 0,
            },
        ];
        assert_eq!(nack(&mut responder, start, ssrc, lost.clone()), 5);
        // A retry one RTT later (the retransmission was lost again) is served.
        let retry = start + Duration::from_millis(60);
        assert_eq!(nack(&mut responder, retry, ssrc, lost), 5);
    }

    #[test]
    fn uncached_gaps_cost_no_budget_and_cannot_crowd_out_repairable_loss() {
        let (mut responder, ssrc) = bound(MediaKind::Camera, false);
        send(&mut responder, ssrc, 64);
        responder.tokens = 5.0;
        // Oldest first, as receivers retry: 340 gaps the SFU never sent, then 60..=63.
        let mut pairs: Vec<_> = (0..20)
            .map(|n| NackPair {
                packet_id: 65000 + n * 17,
                lost_packets: u16::MAX,
            })
            .collect();
        pairs.push(NackPair {
            packet_id: 60,
            lost_packets: 0b111,
        });
        assert_eq!(nack(&mut responder, Instant::now(), ssrc, pairs), 4);
        assert_eq!(responder.tokens, 1.0);
    }

    #[test]
    fn cache_mirror_matches_the_stock_send_buffer() {
        let (mut responder, ssrc) = bound(MediaKind::Camera, false);
        // Gaps, reordering, duplicates, wrap, a jump past the cache and late packets.
        for seq in [
            10_u16, 11, 13, 12, 500, 400, 2000, 1999, 3100, 64000, 65535, 0, 1, 1, 65534, 30000,
            29990, 62000, 61000, 61500, 62001,
        ] {
            responder
                .handle_write(tagged(
                    Instant::now(),
                    Packet::Rtp(rtp::Packet {
                        header: rtp::header::Header {
                            version: 2,
                            ssrc,
                            sequence_number: seq,
                            ..Default::default()
                        },
                        payload: vec![0x65; 4].into(),
                    }),
                ))
                .unwrap();
            while responder.poll_write().is_some() {}
        }
        let mut hits = 0;
        for seq in 0..=u16::MAX {
            responder
                .inner
                .handle_read(tagged(
                    Instant::now(),
                    Packet::Rtcp(vec![Box::new(TransportLayerNack {
                        sender_ssrc: 900,
                        media_ssrc: ssrc,
                        nacks: vec![NackPair {
                            packet_id: seq,
                            lost_packets: 0,
                        }],
                    })]),
                ))
                .unwrap();
            while responder.inner.poll_read().is_some() {}
            let stock = std::iter::from_fn(|| responder.inner.poll_write()).count() == 1;
            assert_eq!(responder.cached[&ssrc].has(seq), stock, "{seq}");
            hits += usize::from(stock);
        }
        assert_eq!(hits, 4); // 61000, 61500, 62000, 62001
    }

    #[test]
    fn cached_camera_nack_floods_are_bounded_per_connection() {
        let (mut responder, ssrc) = bound(MediaKind::Camera, true);
        send(&mut responder, ssrc, 1024);
        let start = Instant::now();
        let mut sent = 0;
        // Ten seconds of 1 ms NACK packets, each naming 544 distinct cached
        // sequence numbers (32 pairs × 17): the stock responder would resend
        // millions of packets. The budget allows the burst plus the refill.
        for ms in 0..10_000_u32 {
            let base = (ms as u16).wrapping_mul(544) % 1024;
            let pairs = (0..32)
                .map(|n| NackPair {
                    packet_id: (base + n * 17) % 1024,
                    lost_packets: u16::MAX,
                })
                .collect();
            let now = start + Duration::from_millis(u64::from(ms));
            sent += nack(&mut responder, now, ssrc, pairs);
        }
        assert!(sent <= (BUDGET * 11.0) as usize, "{sent}");
        assert!(
            sent >= (BUDGET * 9.0) as usize,
            "budget starves recovery: {sent}"
        );
        // Nothing banked beyond one burst after an idle period.
        let idle = start + Duration::from_secs(60);
        let all = (0..64)
            .map(|n| NackPair {
                packet_id: n * 16,
                lost_packets: u16::MAX,
            })
            .collect();
        assert_eq!(nack(&mut responder, idle, ssrc, all), BUDGET as usize);
    }
}
