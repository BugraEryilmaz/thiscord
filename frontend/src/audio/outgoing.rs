//! Native encoded queue metadata; never serialized or exposed over IPC.
use super::{connection::Connection, quality::Count};
use std::time::{Duration, Instant};

pub struct Packet {
    payload: Option<bytes::Bytes>,
    connection: Connection,
    at: Instant,
    revision: u64,
    speech: bool,
}
impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutgoingAudioPacket { .. }")
    }
}
impl Packet {
    pub fn new(payload: bytes::Bytes, connection: Connection, revision: u64, speech: bool) -> Self {
        connection.metrics.add(Count::OutgoingOfferedPackets, 1);
        Self {
            payload: Some(payload),
            connection,
            at: Instant::now(),
            revision,
            speech,
        }
    }
    fn reject(&mut self, reason: Count) {
        if self.payload.take().is_some() {
            self.connection.metrics.add(reason, 1);
        }
    }
    pub fn take(
        mut self,
        now: Instant,
        revision: u64,
        allowed: bool,
        connection: &Connection,
    ) -> Option<bytes::Bytes> {
        if !self.connection.accepts(connection)
            || revision != self.revision
            || (self.speech && !allowed)
        {
            self.reject(Count::OutgoingInvalidatedPackets);
        } else if now.saturating_duration_since(self.at) > Duration::from_millis(100) {
            self.reject(Count::OutgoingExpiredPackets);
        }
        self.payload.take()
    }
}
impl Drop for Packet {
    fn drop(&mut self) {
        self.reject(Count::OutgoingInvalidatedPackets);
    }
}
pub fn enqueue(sender: &tokio::sync::mpsc::Sender<Packet>, packet: Packet) -> bool {
    match sender.try_send(packet) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(mut packet)) => {
            packet.reject(Count::OutgoingFullPackets);
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(mut packet)) => {
            packet.reject(Count::OutgoingInvalidatedPackets);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(c: &Connection) -> Packet {
        Packet::new(
            bytes::Bytes::from_static(b"private encoded content"),
            c.clone(),
            7,
            true,
        )
    }
    #[test]
    fn each_outgoing_drop_has_exactly_one_reason_and_no_payload_in_debug() {
        let c = Connection::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let p = packet(&c);
        assert_eq!(format!("{p:?}"), "OutgoingAudioPacket { .. }");
        assert!(enqueue(&tx, p));
        assert!(!enqueue(&tx, packet(&c)));
        let p = rx.try_recv().unwrap();
        let now = p.at + Duration::from_millis(101);
        assert!(p.take(now, 7, true, &c).is_none());
        let p = packet(&c);
        assert!(p.take(Instant::now(), 8, true, &c).is_none()); // PTT/mute restriction, even if relaxed again
        let p = packet(&c);
        assert!(p.take(Instant::now(), 7, false, &c).is_none());
        let p = packet(&c);
        assert!(
            p.take(Instant::now(), 7, true, &Connection::default())
                .is_none()
        );
        drop(rx);
        assert!(!enqueue(&tx, packet(&c)));
        let t = c.metrics.snapshot().total;
        assert_eq!(
            (
                t.outgoing_offered_packets,
                t.outgoing_full_packets,
                t.outgoing_expired_packets,
                t.outgoing_invalidated_packets
            ),
            (6, 1, 1, 4)
        );
    }
    #[test]
    fn expiry_boundary_silence_and_receiver_shutdown() {
        let c = Connection::default();
        let p = packet(&c);
        let now = p.at + Duration::from_millis(100);
        assert!(p.take(now, 7, true, &c).is_some());
        let p = Packet::new(bytes::Bytes::from_static(b"silence"), c.clone(), 7, false);
        assert!(p.take(Instant::now(), 7, false, &c).is_some());
        let (tx, rx) = tokio::sync::mpsc::channel(5);
        for _ in 0..5 {
            assert!(enqueue(&tx, packet(&c)));
        }
        drop(rx);
        assert_eq!(c.metrics.snapshot().total.outgoing_invalidated_packets, 5);
        c.close();
        assert!(packet(&c).take(Instant::now(), 7, true, &c).is_none());
        assert_eq!(c.metrics.snapshot().total.outgoing_invalidated_packets, 6);
    }
}
