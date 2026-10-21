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

// Never infer a wrap from an arbitrary publisher jump. Retain a fixed replay
// window and an inclusive selection floor; holes within the window remain legal.
const REORDER: u64 = 128;
const MAX_ADVANCE: i64 = 4096;
const CLOCK_IDLE: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(super) struct SequenceWindow {
    high: Option<u64>,
    floor: u64,
    seen: u128,
}
impl SequenceWindow {
    /// Locally confirmed queue loss may span more than half a wire sequence
    /// space. Start a fresh mapping above every reserved index, retaining one
    /// visible loss gap. This is not a publisher-controlled replay-window reset.
    pub fn after_loss(&mut self) -> Option<u16> {
        let next = self.high?.checked_add(2)?;
        if next >= (1_u64 << 48) {
            return None;
        }
        self.floor = next;
        Some(next as u16)
    }

    pub fn accept(&mut self, sequence: u16) -> Option<u64> {
        let Some(high) = self.high else {
            let index = u64::from(sequence);
            self.high = Some(index);
            self.floor = index;
            self.seen = 1;
            return Some(index);
        };
        let delta = i64::from(sequence.wrapping_sub(high as u16) as i16);
        if delta > MAX_ADVANCE {
            return None;
        }
        let index = high.checked_add_signed(delta)?;
        if index < self.floor || index >= (1_u64 << 48) {
            return None;
        }
        if delta > 0 {
            self.seen = self.seen.checked_shl(delta as u32).unwrap_or(0) | 1;
            self.high = Some(index);
        } else {
            let behind = high - index;
            if behind >= REORDER || self.seen & (1 << behind) != 0 {
                return None;
            }
            self.seen |= 1 << behind;
        }
        Some(index)
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
    input: SequenceWindow,
    input_base: u64,
    output_base: u64,
    output_high: Option<u64>,
    clock: Option<(u32, Instant)>,
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
            output_high: self.output_high,
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
            self.clear();
            self.identity = Some(identity);
        }
        // RTP timestamps cannot order separate encoder/share clocks, or pauses
        // approaching the 32-bit half range. Reacquire a fresh IDR after idle.
        if self
            .clock
            .is_some_and(|(_, at)| arrived.saturating_duration_since(at) >= CLOCK_IDLE)
        {
            self.clear();
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
            return self.rewrite(packet, arrived).into_iter().collect();
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
        // Both encoders use one compositor clock. An old buffered IDR cannot
        // move that clock backwards at an upgrade/downgrade.
        if self.clock.is_some_and(|(timestamp, _)| {
            self.pending[0].header.timestamp.wrapping_sub(timestamp) as i32 <= 0
        }) {
            self.discard();
            return vec![];
        }
        self.selected = Some(layer);
        self.input = SequenceWindow::default();
        self.input_base = u64::from(self.pending[0].header.sequence_number);
        self.output_base = self.output_high.map_or(self.input_base, |last| last + 1);
        self.released_at = self.started;
        let packets = std::mem::take(&mut self.pending);
        self.discard();
        packets
            .into_iter()
            .filter_map(|p| self.rewrite(p, arrived))
            .collect()
    }
    fn rewrite(&mut self, mut packet: rtp::Packet, arrived: Instant) -> Option<rtp::Packet> {
        let input = self.input.accept(packet.header.sequence_number)?;
        let output = self
            .output_base
            .checked_add(input.checked_sub(self.input_base)?)?;
        if output >= (1_u64 << 48) {
            return None;
        }
        packet.header.sequence_number = output as u16;
        if self.output_high.is_none_or(|last| output > last) {
            self.output_high = Some(output);
            self.clock = Some((packet.header.timestamp, arrived));
        }
        Some(packet)
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
    use rtc::shared::marshal::Marshal;

    fn idr(seq: u16, ts: u32) -> rtp::Packet {
        packet(
            seq,
            ts,
            &[24, 0, 2, 0x67, 1, 0, 2, 0x68, 1, 0, 2, 0x65, 1],
            true,
        )
    }

    #[test]
    fn encrypted_switches_reordering_wrap_and_resets_never_reuse_packet_indexes() {
        use rtc::srtp::{context::Context, protection_profile::ProtectionProfile};
        let provider = rtc::crypto::default_provider().unwrap();
        for profile in [
            ProtectionProfile::AeadAes128Gcm,
            ProtectionProfile::Aes128CmHmacSha1_80,
        ] {
            let context = || {
                Context::new(
                    &vec![3; profile.key_len()],
                    &vec![4; profile.salt_len()],
                    profile,
                    None,
                    None,
                    provider.crypto(),
                )
                .unwrap()
            };
            let mut encrypt = context();
            let mut decrypt = context();
            let mut indexes = std::collections::HashSet::new();
            let mut previous: Option<u64> = None;
            let mut check = |packets: Vec<rtp::Packet>| {
                for packet in packets {
                    let seq = packet.header.sequence_number;
                    // Independent SRTP packet-index oracle, relative to the last
                    // encrypted packet (including reordered ones), not Route state.
                    let index = previous.map_or(u64::from(seq), |last| {
                        last.checked_add_signed(i64::from(seq.wrapping_sub(last as u16) as i16))
                            .unwrap()
                    });
                    assert!(indexes.insert(index), "reused SRTP index {index}");
                    previous = Some(index);
                    let plain = packet.marshal().unwrap();
                    let encrypted = encrypt.encrypt_rtp(&plain).unwrap();
                    assert_eq!(
                        decrypt.decrypt_rtp(&encrypted).unwrap().as_ref(),
                        plain.as_ref()
                    );
                }
            };
            let mut route = Route::default();
            let mut id = (uuid::Uuid::new_v4(), 9, Instant::now(), 0, 0);
            check(route.push(id, 0, 0, idr(100, 9000)));
            check(route.push(id, 1, 1, idr(500, 12000)));
            // Reviewer's exact collision: low499 must not become primary100.
            assert!(
                route
                    .push(id, 1, 1, packet(499, 12000, &[0x41, 99], true))
                    .is_empty()
            );
            assert!(
                route
                    .push(id, 1, 1, packet(500, 12000, &[0x41, 98], true))
                    .is_empty()
            );
            check(route.push(id, 1, 1, packet(502, 15000, &[0x41, 2], true)));
            check(route.push(id, 1, 1, packet(501, 12000, &[0x41, 1], true)));
            assert!(
                route
                    .push(id, 1, 1, packet(501, 12000, &[0x41, 97], true))
                    .is_empty()
            );
            for seq in [40_000, 20_000, 499, 0] {
                assert!(
                    route
                        .push(id, 1, 1, packet(seq, 18000, &[0x41, 96], true))
                        .is_empty()
                );
            }
            // Several output wraps, input wraps and legitimate hole filling.
            for n in 503_u32..140_000 {
                if n % 3 == 0 {
                    continue;
                }
                check(route.push(id, 1, 1, packet(n as u16, n * 3000, &n.to_be_bytes(), true)));
                if n % 3 == 1 && n > 503 {
                    check(route.push(
                        id,
                        1,
                        1,
                        packet((n - 1) as u16, (n - 1) * 3000, &[0x41, 3], true),
                    ));
                }
            }
            for change in 0..5 {
                route.clear();
                match change {
                    0 => id.0 = uuid::Uuid::new_v4(),
                    1 => id.1 += 1,
                    2 => id.2 = Instant::now(),
                    3 => id.3 += 1,
                    _ => id.4 += 1,
                }
                let output = route.push(id, change % 2, change % 2, idr(65535, 1));
                assert_eq!(output.len(), 1);
                check(output);
                check(route.push(
                    id,
                    change % 2,
                    change % 2,
                    packet(0, 3001, &[0x41, 4], true),
                ));
                assert!(
                    route
                        .push(
                            id,
                            change % 2,
                            change % 2,
                            packet(65534, 1, &[0x41, 95], true)
                        )
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn timestamp_epochs_idle_and_wrap_are_independent_of_sequence_continuity() {
        let mut route = Route::default();
        let mut id = (uuid::Uuid::new_v4(), 9, Instant::now(), 0, 0);
        let now = Instant::now();
        assert_eq!(
            route
                .push_at(id, 0, 0, idr(100, u32::MAX - 3000), now)
                .len(),
            1
        );
        assert_eq!(route.push_at(id, 1, 1, idr(500, 3000), now).len(), 1);
        assert!(route.push_at(id, 0, 0, idr(101, 2000), now).is_empty());
        assert_eq!(route.push_at(id, 0, 0, idr(102, 6000), now).len(), 1);
        let later = now + Duration::from_secs(7 * 3600);
        let output = route.push_at(id, 1, 1, idr(501, 6000 + 7 * 3600 * 90_000), later);
        assert_eq!(output[0].header.sequence_number, 103);
        id.1 += 1;
        let output = route.push_at(id, 0, 0, idr(1, 1), later);
        assert_eq!(output[0].header.sequence_number, 104);
        route.clear();
        let output = route.push_at(id, 0, 0, idr(2, 0), later);
        assert_eq!(output[0].header.sequence_number, 105);
    }
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
            output[0].header.sequence_number,
            (route.output_base + 110 - route.input_base) as u16
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
