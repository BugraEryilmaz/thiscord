//! RFC 2198 forwarding only: bounded parsing and privacy floors, never decoding.
use super::*;
use thiscord_shared::rtp_red as wire;

/// Constant space per membership. The challenge is a globally ordered receiver
/// barrier, but its satisfaction belongs ONLY to this publisher connection.
#[derive(Clone)]
pub(super) struct Barrier {
    pub receiver_floor: u32,
    publication_floor: u32,
    pub receiver_started: Instant,
    pub satisfied: u32,
    pub pending: u32,
    issued: u32,
    last_full_generation: u64,
    sent_at: Option<Instant>,
    accepted_at: Option<Instant>,
    ready_at: Option<Instant>,
    opened_at: Instant,
    attempts: u8,
}
impl Default for Barrier {
    fn default() -> Self {
        Self {
            receiver_floor: 0,
            publication_floor: 0,
            receiver_started: Instant::now(),
            satisfied: 0,
            pending: 0,
            issued: 0,
            last_full_generation: 0,
            sent_at: None,
            accepted_at: None,
            ready_at: None,
            opened_at: Instant::now(),
            attempts: 0,
        }
    }
}
impl Barrier {
    /// Resume the same immutable proof, including an ACK whose Ready was lost.
    /// Only detached time is excluded: repeated reattachments cannot renew the
    /// five-second attached budget or the normal three-transmission retry budget.
    /// The socket owner independently enforces the bounded signaling grace.
    pub fn resume(&mut self, detached_at: Option<Instant>, now: Instant) -> Option<u32> {
        if let Some(detached_at) = detached_at {
            let paused = now.saturating_duration_since(detached_at);
            self.opened_at += paused;
            self.accepted_at = self.accepted_at.map(|at| at + paused);
            self.ready_at = self.ready_at.map(|at| at + paused);
            self.sent_at = self.sent_at.map(|at| at + paused);
        }
        if !self.expired(now)
            && (self.issued > self.satisfied
                || (self.accepted_at.is_some() && self.ready_at.is_none()))
        {
            // One explicit replay on the newly authenticated socket, even if
            // ordinary retries were used on the lost connection.
            self.sent_at = Some(now);
            Some(self.issued)
        } else {
            None
        }
    }

    #[cfg(test)]
    pub fn with_receiver_started(receiver_started: Instant) -> Self {
        Self {
            receiver_started,
            ..Default::default()
        }
    }
    fn challenge(&mut self, challenge: u32, _now: Instant) {
        self.pending = self.pending.max(challenge);
        // Freeze the currently issued challenge, deadline and retries until its
        // response. Keep only the newer maximum waiting behind it. Otherwise a
        // rapidly changing listener could make every healthy publisher reply stale.
    }
    pub fn request(&mut self, now: Instant) -> Option<u32> {
        if self
            .sent_at
            .is_some_and(|sent| now.saturating_duration_since(sent) < Duration::from_millis(250))
        {
            return None;
        }
        if self.issued <= self.satisfied {
            // Measure the service interval only after the client has processed
            // its proof ACK. Delayed/batched ACK delivery cannot consume it.
            if self.accepted_at.is_some() {
                let ready = self.ready_at?;
                if now.saturating_duration_since(ready) < Duration::from_millis(250) {
                    return None;
                }
            }
            if self.pending <= self.satisfied {
                return None;
            }
            self.issued = self.pending;
            self.opened_at = now;
            self.attempts = 0;
            self.accepted_at = None;
            self.ready_at = None;
        }
        if self.attempts >= 3
            || now.saturating_duration_since(self.opened_at) >= Duration::from_secs(2)
        {
            return None;
        }
        self.attempts += 1;
        self.sent_at = Some(now);
        Some(self.issued)
    }
    fn expired(&self, now: Instant) -> bool {
        (self.issued > self.satisfied
            && now.saturating_duration_since(self.opened_at) >= Duration::from_secs(5))
            || (self.ready_at.is_none()
                && self.accepted_at.is_some_and(|accepted| {
                    now.saturating_duration_since(accepted) >= Duration::from_secs(5)
                }))
    }
}

pub(super) fn ready(member: &mut Member, generation: u64, challenge: u32, now: Instant) -> bool {
    if !member.packet_redundancy
        || !member.active.load(Ordering::Acquire)
        || member.expected_sender_generation != Some(generation)
        || challenge == 0
        || challenge != member.red.issued
        || challenge != member.red.satisfied
        || member.red.accepted_at.is_none()
        || member.red.expired(now)
    {
        return false;
    }
    member.red.ready_at.get_or_insert(now);
    true
}

pub(super) fn receives(source: &Member, receiver: &Member) -> bool {
    !source.packet_redundancy
        || !receiver.packet_redundancy
        || source.red.satisfied >= receiver.red.receiver_floor
}

pub(super) fn request(member: &mut Member, now: Instant) -> Option<u32> {
    if !member.packet_redundancy
        || member.expected_sender_generation.is_none()
        || member.red.receiver_floor == 0
        || !member.active.load(Ordering::Acquire)
    {
        return None;
    }
    member.red.request(now)
}

pub(super) fn expire(room: &Room, member: &Member, now: Instant) -> bool {
    if member.active.load(Ordering::Acquire) && member.red.expired(now) {
        member.active.store(false, Ordering::Release);
        room.changed.send_replace(());
        true
    } else {
        false
    }
}

pub(super) fn challenge_publishers(members: &mut HashMap<usize, Member>, receiver_slot: usize) {
    let Some(receiver) = members.get(&receiver_slot) else {
        return;
    };
    if !receiver.packet_redundancy {
        return;
    }
    let floor = receiver.red.receiver_floor;
    let now = Instant::now();
    for (&slot, source) in members.iter_mut() {
        if slot != receiver_slot && source.packet_redundancy {
            source.red.challenge(floor, now);
        }
    }
}

pub(super) fn challenge_new_publisher(members: &mut HashMap<usize, Member>, source_slot: usize) {
    let floor = members
        .iter()
        .filter(|(slot, member)| **slot != source_slot && member.packet_redundancy)
        .map(|(_, member)| member.red.receiver_floor)
        .max()
        .unwrap_or(0);
    if let Some(source) = members
        .get_mut(&source_slot)
        .filter(|source| source.packet_redundancy)
    {
        source.red.challenge(floor, Instant::now());
    }
}

pub(super) fn participant(member: &Member) -> Participant {
    let mut info = member.info.clone();
    if !member.packet_redundancy {
        info.microphone_epoch = 0;
        info.microphone_source_epoch = 0;
    }
    info
}

pub(super) fn register(engine: &mut MediaEngine) {
    engine
        .register_codec(
            rtc::rtp_transceiver::rtp_sender::RTCRtpCodecParameters {
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
        .expect("valid microphone RED codec");
}

pub(super) fn boundary(room: &Room, member: &mut Member) -> bool {
    let Some(epoch) = next_epoch(room) else {
        member.active.store(false, Ordering::Release);
        return false;
    };
    member.info.microphone_epoch = epoch;
    member.microphone_started = Instant::now();
    room.changed.send_replace(());
    true
}

/// A server moderation edge fences both retained receive history and the
/// publisher's encoder/queued UDP. A newer full boundary alone is not proof of
/// observing this edge: require its authenticated challenge response as well.
pub(super) fn moderation_boundary(room: &Room, member: &mut Member) -> bool {
    if !boundary(room, member) {
        return false;
    }
    member.red.receiver_floor = member.info.microphone_epoch;
    member.red.receiver_started = member.microphone_started;
    if member.packet_redundancy {
        member.red.publication_floor = member.info.microphone_epoch;
        member
            .red
            .challenge(member.info.microphone_epoch, Instant::now());
    }
    true
}

pub(super) fn next_epoch(room: &Room) -> Option<u32> {
    room.microphone_epoch
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .ok()
        .and_then(|value| value.checked_add(1))
}

pub(super) fn accept_boundary(room: &Room, member: &mut Member, generation: u64) -> bool {
    if generation == 0
        || !member.packet_redundancy
        || member
            .expected_sender_generation
            .is_some_and(|current| generation < current)
    {
        return false;
    }
    if member.expected_sender_generation == Some(generation) {
        return member.red.last_full_generation == generation;
    }
    if member.expected_sender_generation != Some(generation) {
        if !boundary(room, member) {
            return false;
        }
        member.expected_sender_generation = Some(generation);
        member.red.receiver_floor = member.info.microphone_epoch;
        member.red.receiver_started = member.microphone_started;
        member.red.last_full_generation = generation;
    }
    true
}

pub(super) fn accept_refresh(
    room: &Room,
    member: &mut Member,
    generation: u64,
    challenge: u32,
) -> bool {
    accept_refresh_at(room, member, generation, challenge, Instant::now())
}

pub(super) fn accept_refresh_at(
    room: &Room,
    member: &mut Member,
    generation: u64,
    challenge: u32,
    now: Instant,
) -> bool {
    if !member.packet_redundancy
        || !member.active.load(Ordering::Acquire)
        || generation == 0
        || challenge == 0
        || member.red.receiver_floor == 0
        || challenge != member.red.issued
    {
        return false;
    }
    if member.red.expired(now) {
        return false;
    }
    if challenge <= member.red.satisfied {
        if challenge != member.red.satisfied {
            return false;
        }
        if member.expected_sender_generation == Some(generation) {
            return true;
        }
        // A lost proof ACK can be superseded by a full local boundary. A fresh
        // proof of the same issued challenge may clear history again, but must
        // never renew its original readiness deadline or service cooldown.
        if member
            .expected_sender_generation
            .is_none_or(|current| generation <= current)
            || !boundary(room, member)
        {
            return false;
        }
        member.expected_sender_generation = Some(generation);
        return true;
    }
    if member
        .expected_sender_generation
        .is_some_and(|current| generation <= current)
    {
        return false;
    }
    if !boundary(room, member) {
        return false;
    }
    member.expected_sender_generation = Some(generation);
    member.red.satisfied = challenge;
    member.red.accepted_at = Some(now);
    member.red.ready_at = None;
    // Do not move receiver_floor/receiver_started, or challenge anyone else:
    // answering another listener must not recursively start another barrier.
    true
}

pub(super) fn sender_generation(packet: &rtp::Packet, member: &Member) -> bool {
    if !member.packet_redundancy {
        return true;
    }
    let [hi, lo] = packet.header.csrc.as_slice() else {
        return false;
    };
    member.red.satisfied >= member.red.publication_floor
        && member.expected_sender_generation == Some((u64::from(*hi) << 32) | u64::from(*lo))
}

pub(super) fn valid_ingress(packet: &rtp::Packet, negotiated: bool) -> bool {
    match packet.header.payload_type {
        wire::OPUS_PAYLOAD_TYPE => {
            !packet.payload.is_empty() && packet.payload.len() <= wire::MAX_PAYLOAD
        }
        wire::PAYLOAD_TYPE => negotiated && wire::parse(&packet.payload).is_some(),
        _ => false,
    }
}

/// A floor refers to the publisher's original timestamp space. SFU remapping
/// adds the same timestamp offset to all blocks, so RED offsets stay unchanged.
/// No previous sequence numbers/markers exist in RED; do not manufacture them.
#[derive(Clone, Copy, Default)]
pub(super) struct Floor {
    key: Option<(uuid::Uuid, u32, u32)>,
    timestamp: u32,
    last: Option<Instant>,
}

impl Floor {
    pub(super) fn prepare(
        &mut self,
        packet: &mut rtp::Packet,
        source: uuid::Uuid,
        source_epoch: u32,
        receiver_epoch: u32,
        negotiated: bool,
    ) -> bool {
        if !valid_ingress(packet, true) {
            return false;
        }
        let key = Some((source, source_epoch, receiver_epoch));
        if self.key != key
            || self
                .last
                .is_some_and(|last| last.elapsed() >= Duration::from_secs(6 * 60 * 60))
        {
            self.key = key;
            self.timestamp = packet.header.timestamp;
        }
        if (packet.header.timestamp.wrapping_sub(self.timestamp) as i32) < 0 {
            return false;
        }
        self.last = Some(Instant::now());
        // Keep comparisons unambiguous across repeated RTP clock wraps during
        // long calls. Twelve 20 ms frames match the receiver's bounded jitter
        // capacity; anything older cannot become timely redundant playback.
        let trailing = packet.header.timestamp.wrapping_sub(12 * wire::FRAME);
        if (trailing.wrapping_sub(self.timestamp) as i32) > 0 {
            self.timestamp = trailing;
        }
        if packet.header.payload_type == wire::OPUS_PAYLOAD_TYPE {
            return true;
        }
        let Some(mut red) = wire::parse(&packet.payload) else {
            return false;
        };
        if !negotiated {
            packet.payload = red.primary.to_vec().into();
            packet.header.payload_type = wire::OPUS_PAYLOAD_TYPE;
            return true;
        }
        red.history.retain(|block| {
            (packet
                .header
                .timestamp
                .wrapping_sub(u32::from(block.offset))
                .wrapping_sub(self.timestamp) as i32)
                >= 0
        });
        let Some(payload) = wire::encode(red.primary, &red.history) else {
            return false;
        };
        packet.payload = payload.into();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_replays_exhausted_proof_without_renewing_attached_budget() {
        let now = Instant::now();
        let mut barrier = Barrier::default();
        barrier.challenge(10, now);
        for millis in [0, 250, 500] {
            assert_eq!(
                barrier.request(now + Duration::from_millis(millis)),
                Some(10)
            );
        }
        let detached = now + Duration::from_secs(1);
        let resumed = detached + Duration::from_secs(20);
        assert_eq!(barrier.resume(Some(detached), resumed), Some(10));
        assert_eq!(barrier.attempts, 3);
        assert_eq!(barrier.request(resumed + Duration::from_millis(250)), None);
        assert!(!barrier.expired(resumed + Duration::from_millis(3999)));
        assert!(barrier.expired(resumed + Duration::from_secs(4)));
        assert_eq!(barrier.resume(None, resumed + Duration::from_secs(4)), None);
        assert_eq!(barrier.satisfied, 0);
    }

    #[test]
    fn resume_preserves_ready_deadline_and_does_not_revive_expired_proofs() {
        let now = Instant::now();
        let mut barrier = Barrier::default();
        barrier.challenge(10, now);
        assert_eq!(barrier.request(now), Some(10));
        barrier.satisfied = 10;
        barrier.accepted_at = Some(now);
        barrier.challenge(20, now);
        let detached = now + Duration::from_secs(2);
        let resumed = detached + Duration::from_secs(10);
        assert_eq!(barrier.resume(Some(detached), resumed), Some(10));
        assert_eq!(barrier.request(resumed), None, "Ready still required");
        assert!(!barrier.expired(resumed + Duration::from_millis(2999)));
        assert!(barrier.expired(resumed + Duration::from_secs(3)));
        let detached = resumed + Duration::from_secs(3);
        assert_eq!(
            barrier.resume(Some(detached), detached + Duration::from_secs(10)),
            None
        );
        assert!(barrier.expired(detached + Duration::from_secs(10)));
    }

    #[test]
    fn refresh_requests_freeze_issued_budget_and_bound_cadence_under_listener_flood() {
        let now = Instant::now();
        let mut barrier = Barrier::default();
        barrier.challenge(10, now);
        assert_eq!(barrier.request(now), Some(10));
        for n in 11..1000 {
            barrier.challenge(n, now + Duration::from_millis(1));
        }
        assert_eq!(barrier.request(now + Duration::from_millis(249)), None);
        assert_eq!(barrier.request(now + Duration::from_millis(250)), Some(10));
        assert_eq!(barrier.request(now + Duration::from_millis(500)), Some(10));
        assert_eq!(barrier.request(now + Duration::from_millis(750)), None);
        assert_eq!(barrier.pending, 999);
        assert!(!barrier.expired(now + Duration::from_millis(4999)));
        assert!(barrier.expired(now + Duration::from_secs(5)));
        assert_eq!(barrier.satisfied, 0, "timeout cannot imply permission");
        // A responsive source completes its frozen challenge despite the flood.
        barrier.satisfied = 10;
        assert!(!barrier.expired(now + Duration::from_secs(5)));
        assert_eq!(
            barrier.request(now + Duration::from_millis(501)),
            None,
            "ACK bypassed issue cadence"
        );
        assert_eq!(barrier.request(now + Duration::from_millis(750)), Some(999));
        assert!(!barrier.expired(now + Duration::from_millis(5749)));
        assert!(barrier.expired(now + Duration::from_millis(5750)));
    }

    fn packet(timestamp: u32) -> rtp::Packet {
        rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type: wire::PAYLOAD_TYPE,
                sequence_number: 65535,
                timestamp,
                marker: true,
                ..Default::default()
            },
            payload: wire::encode(
                &[0x78, 3, 4],
                &[
                    wire::Block {
                        offset: 1920,
                        payload: &[0x78, 1, 2],
                    },
                    wire::Block {
                        offset: 960,
                        payload: &[0x78, 2, 3],
                    },
                ],
            )
            .unwrap()
            .into(),
        }
    }

    #[test]
    fn unnegotiated_and_malformed_ingress_is_rejected() {
        let mut p = packet(4000);
        assert!(!valid_ingress(&p, false));
        assert!(valid_ingress(&p, true));
        p.payload = vec![0xff; 1101].into();
        assert!(!valid_ingress(&p, true));
        p.header.payload_type = 111;
        assert!(!valid_ingress(&p, true));
        p.payload = vec![0x78, 1, 2].into();
        assert!(valid_ingress(&p, false));
        p.header.payload_type = 96;
        assert!(!valid_ingress(&p, true));
    }

    #[test]
    fn fallback_preserves_primary_and_rtp_identity() {
        let mut p = packet(4000);
        let header = p.header.clone();
        assert!(Floor::default().prepare(&mut p, uuid::Uuid::nil(), 1, 1, false));
        assert_eq!(&p.payload[..], &[0x78, 3, 4]);
        assert_eq!(p.header.payload_type, 111);
        assert_eq!(p.header.timestamp, header.timestamp);
        assert_eq!(p.header.sequence_number, header.sequence_number);
        assert_eq!(p.header.marker, header.marker);
    }

    #[test]
    fn floors_filter_history_after_every_privacy_boundary_and_reordering() {
        let mut floor = Floor::default();
        let id = uuid::Uuid::new_v4();
        for (id, source_epoch, receiver_epoch) in [
            (id, 1, 1),
            (id, 2, 1),
            (id, 2, 2),
            (uuid::Uuid::new_v4(), 2, 2),
        ] {
            let mut first = packet(u32::MAX - 959);
            assert!(floor.prepare(&mut first, id, source_epoch, receiver_epoch, true));
            assert!(wire::parse(&first.payload).unwrap().history.is_empty());
            let mut next = packet(0);
            assert!(floor.prepare(&mut next, id, source_epoch, receiver_epoch, true));
            assert_eq!(wire::parse(&next.payload).unwrap().history.len(), 1);
            let mut third = packet(960);
            assert!(floor.prepare(&mut third, id, source_epoch, receiver_epoch, true));
            assert_eq!(wire::parse(&third.payload).unwrap().history.len(), 2);
            // A reordered eligible primary remains eligible, but nothing before
            // the boundary can restart or escape it.
            assert!(floor.prepare(&mut next, id, source_epoch, receiver_epoch, true));
            let mut stale = packet(u32::MAX - 1919);
            assert!(!floor.prepare(&mut stale, id, source_epoch, receiver_epoch, true));
        }
    }

    #[test]
    fn raw_opus_dtx_gaps_and_timestamp_remap_preserve_offsets() {
        let mut floor = Floor::default();
        let id = uuid::Uuid::nil();
        let mut raw = packet(0);
        raw.header.payload_type = 111;
        raw.payload = vec![0x78, 1, 2].into();
        assert!(floor.prepare(&mut raw, id, 1, 1, true));
        let mut resumed = packet(48_000 * 31);
        resumed.header.sequence_number = 0;
        assert!(floor.prepare(&mut resumed, id, 1, 1, true));
        let before = wire::parse(&resumed.payload).unwrap();
        let original = resumed.header.timestamp;
        resumed.header.timestamp = resumed.header.timestamp.wrapping_add(u32::MAX - 400);
        for b in before.history {
            assert_eq!(
                resumed.header.timestamp.wrapping_sub(u32::from(b.offset)),
                original
                    .wrapping_sub(u32::from(b.offset))
                    .wrapping_add(u32::MAX - 400)
            );
        }
        assert_eq!(resumed.header.sequence_number, 0);
    }

    #[test]
    fn long_calls_slide_floor_across_multiple_timestamp_wraps() {
        let mut floor = Floor::default();
        let id = uuid::Uuid::nil();
        for second in 0..(60 * 60 * 30u32) {
            let timestamp = second.wrapping_mul(48_000);
            let mut p = packet(timestamp);
            assert!(floor.prepare(&mut p, id, 1, 1, true));
            if second > 0 {
                let mut reorder = packet(timestamp.wrapping_sub(960));
                assert!(floor.prepare(&mut reorder, id, 1, 1, true));
                let mut late = packet(timestamp.wrapping_sub(13 * 960));
                assert!(!floor.prepare(&mut late, id, 1, 1, true));
            }
        }
        floor.last = Some(Instant::now() - Duration::from_secs(6 * 60 * 60));
        let mut resumed = packet(0x8000_0000);
        assert!(floor.prepare(&mut resumed, id, 1, 1, true));
        assert!(wire::parse(&resumed.payload).unwrap().history.is_empty());
    }
}
