//! The stock responder emits cached packets below application authorization and
//! byte accounting. Screen streams therefore use explicit IDR recovery only.
use super::MediaKind;
use rtc::{
    interceptor::{
        Interceptor, NackResponderBuilder, NackResponderInterceptor, Packet, StreamInfo,
        TaggedPacket,
    },
    rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack,
    sansio::Protocol,
    shared::error::Error,
};
use std::time::Instant;

pub(super) struct Responder(NackResponderInterceptor);
impl Default for Responder {
    fn default() -> Self {
        Self(NackResponderBuilder::new().build())
    }
}

fn screen(ssrc: u32) -> bool {
    MediaKind::from_relay_ssrc(ssrc)
        .is_some_and(|(kind, _)| matches!(kind, MediaKind::ScreenVideo | MediaKind::SystemAudio))
}

impl Protocol<TaggedPacket, TaggedPacket, ()> for Responder {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Error> {
        if let Packet::Rtcp(packets) = &mut msg.message.packet {
            // Discard before the stock responder expands NACK bitmaps or queues
            // retransmits. No per-request allocation, cache or recovery work.
            packets.retain(|packet| {
                !packet
                    .as_any()
                    .downcast_ref::<TransportLayerNack>()
                    .is_some_and(|nack| screen(nack.media_ssrc))
            });
            if packets.is_empty() {
                return Ok(());
            }
        }
        self.0.handle_read(msg)
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.0.poll_read()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Error> {
        self.0.handle_write(msg)
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.0.poll_write()
    }
    fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.0.handle_timeout(now)
    }
    fn poll_timeout(&mut self) -> Option<Instant> {
        self.0.poll_timeout()
    }
}

impl Interceptor for Responder {
    fn bind_local_stream(&mut self, info: &StreamInfo) {
        // SSRC-based, not codec-based: camera also uses H.264. Never create a
        // screen cache, even if a peer negotiates NACK/RTX for that codec.
        if !screen(info.ssrc) {
            self.0.bind_local_stream(info);
        }
    }
    fn unbind_local_stream(&mut self, info: &StreamInfo) {
        self.0.unbind_local_stream(info);
    }
    fn bind_remote_stream(&mut self, info: &StreamInfo) {
        self.0.bind_remote_stream(info);
    }
    fn unbind_remote_stream(&mut self, info: &StreamInfo) {
        self.0.unbind_remote_stream(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::rtcp::transport_feedbacks::transport_layer_nack::NackPair;
    use rtc::{
        interceptor::{AttributedPacket, RTCPFeedback},
        rtp,
    };

    fn tagged(packet: Packet) -> TaggedPacket {
        TaggedPacket {
            now: Instant::now(),
            transport: Default::default(),
            message: AttributedPacket::new(packet),
        }
    }

    #[test]
    fn screen_streams_never_cache_or_expand_feedback_even_with_rtx_negotiated() {
        for kind in [
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
            MediaKind::Camera,
            MediaKind::Microphone,
        ] {
            for rtx in [false, true] {
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
                responder
                    .handle_write(tagged(Packet::Rtp(rtp::Packet {
                        header: rtp::header::Header {
                            version: 2,
                            ssrc: info.ssrc,
                            sequence_number: 7,
                            ..Default::default()
                        },
                        payload: vec![0x65; 1200].into(),
                    })))
                    .unwrap();
                assert!(responder.poll_write().is_some()); // Ordinary media always passes.
                for _ in 0..100 {
                    responder
                        .handle_read(tagged(Packet::Rtcp(vec![Box::new(TransportLayerNack {
                            sender_ssrc: 900,
                            media_ssrc: info.ssrc,
                            nacks: vec![
                                NackPair {
                                    packet_id: 7,
                                    lost_packets: u16::MAX
                                };
                                32
                            ],
                        })])))
                        .unwrap();
                    if screen(info.ssrc) {
                        assert!(responder.poll_read().is_none());
                        assert!(responder.poll_write().is_none());
                    } else {
                        assert!(responder.poll_read().is_some());
                        for _ in 0..32 {
                            assert!(responder.poll_write().is_some());
                        }
                        assert!(responder.poll_write().is_none());
                    }
                }
                responder.unbind_local_stream(&info);
            }
        }
    }
}
