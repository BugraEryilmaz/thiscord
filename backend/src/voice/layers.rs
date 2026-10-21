//! Per-viewer compressed layer selection. No transcoding; switches release only
//! a contiguous SPS/PPS prefix followed by the start of an IDR. The relay SSRC
//! stays stable; ordinary loss recovery handles loss later in that IDR.
use super::*;

/// Both screen ingress tracks share the original publisher traffic ceiling.
/// A second SSRC must not double the per-account ingress allocation.
pub(super) struct IngressBudget {
    since: Instant,
    packets: usize,
    bytes: usize,
}
impl Default for IngressBudget {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            packets: 0,
            bytes: 0,
        }
    }
}
impl IngressBudget {
    pub fn admit(&mut self, bytes: usize) -> bool {
        if self.since.elapsed() >= Duration::from_secs(1) {
            *self = Self::default();
        }
        self.packets = self.packets.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.packets <= thiscord_shared::screen::MAX_PACKETS_PER_SECOND
            && self.bytes <= thiscord_shared::screen::MAX_BYTES_PER_SECOND
    }
}

#[derive(Default)]
pub(super) struct Route {
    identity: Option<(uuid::Uuid, u32, Instant, u64, u64)>,
    selected: Option<usize>,
    pub released_at: Option<Instant>,
    desired: usize,
    pending: Vec<rtp::Packet>,
    bytes: usize,
    started: Option<Instant>,
    nals: u8,
    fragmented: Option<u8>,
    offset: u16,
    last: Option<(u16, u32)>,
}

impl Route {
    pub fn expire(&mut self) {
        if self
            .started
            .is_some_and(|at| at.elapsed() > Duration::from_millis(350))
        {
            self.discard();
            self.pending.shrink_to_fit();
        }
    }
    pub fn clear(&mut self) {
        // Subscription/epoch changes invalidate dependencies, never the SRTP
        // sequence space of this viewer's fixed relay SSRC.
        *self = Self {
            identity: self.identity,
            last: self.last,
            ..Self::default()
        };
    }
    fn discard(&mut self) {
        self.pending.clear();
        self.bytes = 0;
        self.started = None;
        self.nals = 0;
        self.fragmented = None;
    }
    #[cfg(test)]
    pub fn push(
        &mut self,
        identity: (uuid::Uuid, u32, Instant, u64, u64),
        desired: usize,
        layer: usize,
        packet: rtp::Packet,
    ) -> Vec<rtp::Packet> {
        self.push_at(identity, desired, layer, packet, Instant::now())
    }
    pub fn push_at(
        &mut self,
        identity: (uuid::Uuid, u32, Instant, u64, u64),
        desired: usize,
        layer: usize,
        packet: rtp::Packet,
        arrived: Instant,
    ) -> Vec<rtp::Packet> {
        if self.identity != Some(identity) {
            if self.identity.is_some_and(|old| old.0 != identity.0) {
                self.last = None;
            }
            self.clear();
            self.identity = Some(identity);
        }
        if self.desired != desired {
            self.desired = desired;
            self.selected = None;
            self.discard();
        }
        if layer != desired {
            return vec![];
        }
        if self.selected == Some(layer) {
            self.released_at = Some(arrived);
            return vec![self.rewrite(packet)];
        }
        if self
            .started
            .is_some_and(|at| at.elapsed() > Duration::from_millis(350))
            || self.pending.last().is_some_and(|last| {
                last.header.timestamp != packet.header.timestamp
                    || last.header.sequence_number.wrapping_add(1) != packet.header.sequence_number
            })
        {
            self.discard();
        }
        if self.pending.is_empty() {
            self.started = Some(arrived);
        }
        if !self.inspect(&packet.payload)
            || self.bytes + packet.payload.len() > thiscord_shared::screen::MAX_FRAME_BYTES
            || self.pending.len() >= 4096
        {
            self.discard();
            return vec![];
        }
        self.bytes += packet.payload.len();
        let marker = packet.header.marker;
        self.pending.push(packet);
        if self.nals != 7 {
            if marker {
                self.discard();
            }
            return vec![];
        }
        self.offset = self.last.map_or(0, |last| {
            last.0
                .wrapping_add(1)
                .wrapping_sub(self.pending[0].header.sequence_number)
        });
        // Both encoders use one compositor clock. An old buffered IDR cannot
        // move that clock backwards at an upgrade/downgrade.
        if self
            .last
            .is_some_and(|last| self.pending[0].header.timestamp.wrapping_sub(last.1) as i32 <= 0)
        {
            self.discard();
            return vec![];
        }
        self.selected = Some(layer);
        self.released_at = self.started;
        let packets = std::mem::take(&mut self.pending);
        self.discard();
        packets.into_iter().map(|p| self.rewrite(p)).collect()
    }
    fn rewrite(&mut self, mut packet: rtp::Packet) -> rtp::Packet {
        packet.header.sequence_number = packet.header.sequence_number.wrapping_add(self.offset);
        if self
            .last
            .is_none_or(|last| packet.header.sequence_number.wrapping_sub(last.0) as i16 > 0)
        {
            self.last = Some((packet.header.sequence_number, packet.header.timestamp));
        }
        packet
    }
    fn nal(&mut self, kind: u8) -> bool {
        self.nals |= match kind {
            7 => 1,
            8 if self.nals & 1 != 0 => 2,
            5 if self.nals & 3 == 3 => 4,
            5 | 8 => return false,
            1..=4 => return false,
            _ => 0,
        };
        true
    }
    fn inspect(&mut self, payload: &[u8]) -> bool {
        let Some(&first) = payload.first() else {
            return false;
        };
        match first & 31 {
            1..=23 => self.fragmented.is_none() && self.nal(first & 31),
            24 => {
                if self.fragmented.is_some() {
                    return false;
                }
                let mut rest = &payload[1..];
                while rest.len() >= 3 {
                    let length = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
                    if length == 0 || length + 2 > rest.len() || !self.nal(rest[2] & 31) {
                        return false;
                    }
                    rest = &rest[length + 2..];
                }
                rest.is_empty()
            }
            28 if payload.len() > 2 => {
                let head = payload[1];
                let kind = head & 31;
                if head & 0x80 != 0 {
                    if self.fragmented.is_some() || head & 0x40 != 0 || !self.nal(kind) {
                        return false;
                    }
                    self.fragmented = Some(kind);
                } else if self.fragmented != Some(kind) {
                    return false;
                }
                if head & 0x40 != 0 {
                    self.fragmented = None;
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffered_prefix_keeps_ingress_age_and_cannot_cross_authorization_epochs() {
        let id = (uuid::Uuid::new_v4(), 9, Instant::now(), 4, 8);
        let mut route = Route::default();
        let arrived = Instant::now() - Duration::from_millis(20);
        route.push_at(id, 0, 0, packet(1, 9000, &[0x67, 1], false), arrived);
        route.push_at(id, 0, 0, packet(2, 9000, &[0x68, 1], false), Instant::now());
        assert_eq!(
            route
                .push_at(id, 0, 0, packet(3, 9000, &[0x65, 1], true), Instant::now())
                .len(),
            3
        );
        assert_eq!(route.released_at, Some(arrived));
        for changed in [(id.0, id.1, id.2, 5, 8), (id.0, id.1, id.2, 4, 9)] {
            route.clear();
            route.push(id, 0, 0, packet(4, 12000, &[0x67, 1], false));
            route.push(id, 0, 0, packet(5, 12000, &[0x68, 1], false));
            assert!(
                route
                    .push(changed, 0, 0, packet(6, 12000, &[0x65, 1], true))
                    .is_empty()
            );
        }
    }
    #[test]
    fn screen_ssrcs_share_one_ingress_budget() {
        let mut budget = IngressBudget::default();
        for _ in 0..thiscord_shared::screen::MAX_PACKETS_PER_SECOND {
            assert!(budget.admit(1));
        }
        assert!(!budget.admit(1));
        budget.since -= Duration::from_secs(1);
        assert!(budget.admit(thiscord_shared::screen::MAX_BYTES_PER_SECOND));
        assert!(!budget.admit(1));
    }
    fn packet(seq: u16, ts: u32, payload: &[u8], marker: bool) -> rtp::Packet {
        rtp::Packet {
            header: rtp::header::Header {
                version: 2,
                ssrc: 20_000,
                sequence_number: seq,
                timestamp: ts,
                marker,
                csrc: vec![9],
                ..Default::default()
            },
            payload: payload.to_vec().into(),
        }
    }
    fn recovery(
        route: &mut Route,
        id: (uuid::Uuid, u32, Instant, u64, u64),
        layer: usize,
        seq: u16,
        ts: u32,
    ) -> Vec<rtp::Packet> {
        assert!(
            route
                .push(id, layer, layer, packet(seq, ts, &[0x67, 1], false))
                .is_empty()
        );
        assert!(
            route
                .push(
                    id,
                    layer,
                    layer,
                    packet(seq.wrapping_add(1), ts, &[0x68, 1], false)
                )
                .is_empty()
        );
        route.push(
            id,
            layer,
            layer,
            packet(seq.wrapping_add(2), ts, &[0x7c, 0x85, 1], false),
        )
    }
    #[test]
    fn fast_and_slow_routes_select_independent_encoders_and_switch_only_at_idr() {
        let id = (uuid::Uuid::new_v4(), 9, Instant::now(), 0, 0);
        let mut fast = Route::default();
        let mut slow = Route::default();
        assert_eq!(recovery(&mut fast, id, 0, 65533, 9000).len(), 3);
        assert_eq!(recovery(&mut slow, id, 1, 100, 9000).len(), 3);
        assert!(
            fast.push(id, 0, 1, packet(103, 9000, &[0x7c, 0x45, 1], true))
                .is_empty()
        );
        assert!(
            slow.push(id, 1, 0, packet(0, 9000, &[0x7c, 0x45, 1], true))
                .is_empty()
        );
        assert_eq!(
            fast.push(id, 0, 0, packet(0, 9000, &[0x7c, 0x45, 1], true))[0]
                .header
                .sequence_number,
            0
        );
        // Upgrade refuses a delta; the new encoder has an unrelated sequence.
        assert!(
            slow.push(id, 0, 0, packet(1, 12000, &[0x41, 1], true))
                .is_empty()
        );
        let switched = recovery(&mut slow, id, 0, 2, 15000);
        assert_eq!(switched.len(), 3);
        assert_eq!(switched[0].header.sequence_number, 103);
        assert!(
            switched
                .iter()
                .all(|p| p.header.ssrc == 20_000 && p.header.csrc == [9])
        );
        // A pause, renewed subscription, and source epoch do not reuse SRTP sequence numbers.
        slow.clear();
        let renewed = (id.0, 10, Instant::now(), 0, 0);
        let switched = recovery(&mut slow, renewed, 1, 200, 18000);
        assert_eq!(switched[0].header.sequence_number, 106);
    }
    #[test]
    fn missing_prefix_gap_stale_idr_and_expired_prefix_never_switch() {
        let id = (uuid::Uuid::new_v4(), 9, Instant::now(), 0, 0);
        let mut route = Route::default();
        assert!(
            route
                .push(id, 0, 0, packet(1, 9000, &[0x65, 1], true))
                .is_empty()
        );
        assert!(
            route
                .push(id, 0, 0, packet(2, 12000, &[0x67, 1], false))
                .is_empty()
        );
        assert!(
            route
                .push(id, 0, 0, packet(4, 12000, &[0x68, 1], false))
                .is_empty()
        );
        assert!(
            route
                .push(id, 0, 0, packet(5, 12000, &[0x65, 1], true))
                .is_empty()
        );
        assert_eq!(recovery(&mut route, id, 0, 6, 15000).len(), 3);
        assert!(recovery(&mut route, id, 1, 100, 12000).is_empty());
        assert!(
            route
                .push(id, 1, 1, packet(103, 18000, &[0x67, 1], false))
                .is_empty()
        );
        route.started = Some(Instant::now() - Duration::from_secs(1));
        assert!(
            route
                .push(id, 1, 1, packet(104, 18000, &[0x68, 1], false))
                .is_empty()
        );
        assert!(
            route
                .push(id, 1, 1, packet(105, 18000, &[0x65, 1], true))
                .is_empty()
        );
        assert_eq!(recovery(&mut route, id, 1, 106, 21000).len(), 3);
        // Ordinary selected-layer packet loss must stay visible to the receiver.
        let output = route.push(id, 1, 1, packet(110, 24000, &[0x41, 1], true));
        assert_eq!(
            output[0].header.sequence_number.wrapping_sub(route.offset),
            110
        );
    }
    #[test]
    fn malformed_fragment_and_memory_pressure_are_bounded() {
        let id = (uuid::Uuid::new_v4(), 9, Instant::now(), 0, 0);
        let mut route = Route::default();
        assert!(
            route
                .push(id, 0, 0, packet(1, 1, &[0x7c, 0x45, 1], true))
                .is_empty()
        );
        for seq in 0..5000 {
            route.push(id, 0, 0, packet(seq, 100, &[0x06; 1400], false));
            assert!(route.bytes <= thiscord_shared::screen::MAX_FRAME_BYTES);
            assert!(route.pending.len() <= 4096);
        }
        route.clear();
        assert_eq!(route.bytes, 0);
        assert!(route.pending.is_empty());
    }
}
