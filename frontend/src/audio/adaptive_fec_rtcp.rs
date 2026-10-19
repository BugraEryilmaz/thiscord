//! Observe only the server's authenticated RTCP blocks for our microphone.
//! The interceptor runs after SRTP authentication, before the RTCP terminus.
use super::super::connection::Connection;
use rtc::{
    interceptor::{Interceptor, Packet, Registry, Slot, StreamInfo, TaggedPacket},
    rtcp::{receiver_report::ReceiverReport, sender_report::SenderReport},
    sansio::Protocol,
    shared::error::Error,
};
use std::{collections::VecDeque, time::Instant};

/// Keep the normal interceptor chain, including all media/report behavior.
pub fn register(registry: Registry, connection: Connection) -> Registry {
    registry.with(
        Slot::from(0),
        Observer {
            connection,
            read: VecDeque::new(),
            write: VecDeque::new(),
        },
    )
}
struct Observer {
    connection: Connection,
    read: VecDeque<TaggedPacket>,
    write: VecDeque<TaggedPacket>,
}
// The chain drains these synchronously between walks; a hard limit also bounds
// a malicious/custom producer. Ordinary 4K pacing batches fit comfortably.
const CAPACITY: usize = 1024;
fn enqueue(queue: &mut VecDeque<TaggedPacket>, packet: TaggedPacket) -> Result<(), Error> {
    if queue.len() >= CAPACITY {
        return Err(Error::Other(
            "RTCP observer pass-through capacity exceeded".into(),
        ));
    }
    queue.push_back(packet);
    Ok(())
}
impl Protocol<TaggedPacket, TaggedPacket, ()> for Observer {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = Error;
    type Time = Instant;
    fn handle_read(&mut self, packet: TaggedPacket) -> Result<(), Error> {
        if self.connection.active()
            && let Packet::Rtcp(packets) = &packet.message.packet
        {
            // Compound RTCP and its report blocks are untrusted. No payload is
            // copied or logged; cap scanning even for artificially large inputs.
            for packet in packets.iter().take(32) {
                let reports = if let Some(rr) = packet.as_any().downcast_ref::<ReceiverReport>() {
                    &rr.reports
                } else if let Some(sr) = packet.as_any().downcast_ref::<SenderReport>() {
                    &sr.reports
                } else {
                    continue;
                };
                if let Some(report) = reports.iter().take(31).find(|r| {
                    r.ssrc == thiscord_shared::voice::MediaKind::Microphone.publisher_ssrc()
                }) {
                    self.connection.observe_uplink(report, Instant::now());
                    break;
                }
            }
        }
        enqueue(&mut self.read, packet)
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.read.pop_front()
    }
    fn handle_write(&mut self, packet: TaggedPacket) -> Result<(), Error> {
        enqueue(&mut self.write, packet)
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.write.pop_front()
    }
}
impl Interceptor for Observer {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::{interceptor::AttributedPacket, rtcp::reception_report::ReceptionReport};
    use thiscord_shared::voice::MediaKind;
    fn packet(message: Packet) -> TaggedPacket {
        TaggedPacket {
            now: Instant::now(),
            transport: Default::default(),
            message: AttributedPacket::new(message),
        }
    }
    fn report(ssrc: u32) -> ReceptionReport {
        ReceptionReport {
            ssrc,
            last_sequence_number: 50,
            total_lost: 10,
            ..Default::default()
        }
    }
    fn observer() -> Observer {
        let connection = Connection::default();
        connection.uplink_sent(0, 1);
        connection.uplink_sent(0, 100);
        Observer {
            connection,
            read: VecDeque::new(),
            write: VecDeque::new(),
        }
    }
    #[test]
    fn reception_blocks_select_microphone_and_preserve_passthrough() {
        for sender_report in [false, true] {
            let mut o = observer();
            let blocks = vec![
                report(MediaKind::SystemAudio.publisher_ssrc()),
                report(MediaKind::Microphone.publisher_ssrc()),
            ];
            let rtcp: Box<dyn rtc::rtcp::Packet> = if sender_report {
                Box::new(SenderReport {
                    reports: blocks,
                    ..Default::default()
                })
            } else {
                Box::new(ReceiverReport {
                    reports: blocks,
                    ..Default::default()
                })
            };
            o.handle_read(packet(Packet::Rtcp(vec![rtcp]))).unwrap();
            assert!(o.connection.uplink_sample(0).is_some());
            assert!(
                matches!(o.poll_read().unwrap().message.packet, Packet::Rtcp(p) if p.len() == 1)
            );
            assert!(o.poll_read().is_none());
            let rtp = rtc::rtp::Packet {
                payload: bytes::Bytes::from_static(b"test"),
                ..Default::default()
            };
            o.handle_read(packet(Packet::Rtp(rtp.clone()))).unwrap();
            assert!(matches!(o.poll_read().unwrap().message.packet, Packet::Rtp(p) if p == rtp));
            o.handle_write(packet(Packet::Rtp(rtp.clone()))).unwrap();
            assert!(matches!(o.poll_write().unwrap().message.packet, Packet::Rtp(p) if p == rtp));
            o.connection.close();
            o.handle_read(packet(Packet::Rtcp(vec![Box::new(ReceiverReport {
                reports: vec![report(MediaKind::Microphone.publisher_ssrc())],
                ..Default::default()
            })])))
            .unwrap();
            assert!(o.connection.uplink_sample(0).is_none());
        }
    }
    #[test]
    fn unrelated_reports_scan_and_pass_through_storage_are_bounded() {
        let mut o = observer();
        let mut reports = vec![report(0); 31];
        reports.push(report(MediaKind::Microphone.publisher_ssrc()));
        o.handle_read(packet(Packet::Rtcp(vec![Box::new(ReceiverReport {
            reports,
            ..Default::default()
        })])))
        .unwrap();
        assert!(o.connection.uplink_sample(0).is_none());
        o.poll_read();
        let mut packets: Vec<Box<dyn rtc::rtcp::Packet>> = (0..32)
            .map(|_| Box::new(ReceiverReport::default()) as Box<dyn rtc::rtcp::Packet>)
            .collect();
        packets.push(Box::new(ReceiverReport {
            reports: vec![report(MediaKind::Microphone.publisher_ssrc())],
            ..Default::default()
        }));
        o.handle_read(packet(Packet::Rtcp(packets))).unwrap();
        assert!(o.connection.uplink_sample(0).is_none());
        o.poll_read();
        for _ in 0..CAPACITY {
            o.handle_write(packet(Packet::Rtp(Default::default())))
                .unwrap();
        }
        assert!(
            o.handle_write(packet(Packet::Rtp(Default::default())))
                .is_err()
        );
        assert_eq!(o.write.len(), CAPACITY);
        for _ in 0..CAPACITY {
            assert!(o.poll_write().is_some());
        }
        assert!(o.poll_write().is_none());
    }
}
