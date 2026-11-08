//! Routing datagrams on a shared shard socket to their peer connection.
//!
//! Every peer on a shard shares one UDP port, so a datagram is attributed by its
//! source address. An address is learned in two ways only: an inbound STUN
//! Binding request whose USERNAME names a live peer's local ICE ufrag (RFC 8445
//! §7.2.2: "recipient:sender"), or a datagram the peer's own ICE agent addressed
//! there. Anything else from an unknown address is dropped before it reaches a
//! DTLS or SRTP stack. A Binding request is authoritative even from a known
//! address, so a reused client port moves to the peer whose credentials it uses.
//! Outbound traffic only claims addresses no peer owns: a client's SDP may list
//! any candidate, including another member's address, which must not move it.
use super::PeerId;
use std::{collections::HashMap, net::SocketAddr};

/// Bounds addresses learned per peer; ICE uses a handful of pairs.
pub(super) const MAX_ADDRESSES_PER_PEER: usize = 16;
const STUN_HEADER: usize = 20;
const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const BINDING_REQUEST: u16 = 0x0001;
const USERNAME: u16 = 0x0006;

/// The local ufrag named by a STUN Binding request, without allocating.
pub(super) fn binding_request_ufrag(datagram: &[u8]) -> Option<&str> {
    if datagram.len() < STUN_HEADER
        || datagram[0] & 0xC0 != 0
        || datagram[4..8] != MAGIC_COOKIE
        || u16::from_be_bytes([datagram[0], datagram[1]]) != BINDING_REQUEST
    {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([datagram[2], datagram[3]]));
    let body = datagram.get(STUN_HEADER..STUN_HEADER.checked_add(length)?)?;
    let mut offset = 0;
    while offset + 4 <= body.len() {
        let kind = u16::from_be_bytes([body[offset], body[offset + 1]]);
        let size = usize::from(u16::from_be_bytes([body[offset + 2], body[offset + 3]]));
        let value = body.get(offset + 4..offset + 4 + size)?;
        if kind == USERNAME {
            let username = std::str::from_utf8(value).ok()?;
            return username.split_once(':').map(|(local, _)| local);
        }
        offset += 4 + size.div_ceil(4) * 4;
    }
    None
}

/// The local ICE ufrag of an SDP description (bundled: one per session).
pub(super) fn sdp_ufrag(sdp: &str) -> Option<String> {
    sdp.lines()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix("a=ice-ufrag:"))
        .map(|ufrag| ufrag.trim().to_owned())
}

#[derive(Default)]
struct Routes {
    ufrag: Option<String>,
    addresses: Vec<SocketAddr>,
}

#[derive(Default)]
pub(super) struct Demux {
    addresses: HashMap<SocketAddr, PeerId>,
    ufrags: HashMap<String, PeerId>,
    peers: HashMap<PeerId, Routes>,
}

impl Demux {
    /// Routes `ufrag` to `peer`. An ICE restart registers a new ufrag: the old
    /// one stops routing, while learned addresses keep the current path alive.
    pub fn register(&mut self, peer: PeerId, ufrag: String) {
        self.ufrags.insert(ufrag.clone(), peer);
        let previous = self.peers.entry(peer).or_default().ufrag.replace(ufrag);
        if let Some(previous) = previous
            && self.ufrags.get(&previous) == Some(&peer)
            && self.peers[&peer].ufrag.as_ref() != Some(&previous)
        {
            self.ufrags.remove(&previous);
        }
    }

    /// The peer for a datagram received from `from`.
    pub fn route(&mut self, from: SocketAddr, datagram: &[u8]) -> Option<PeerId> {
        if let Some(ufrag) = binding_request_ufrag(datagram)
            && let Some(&peer) = self.ufrags.get(ufrag)
        {
            self.learn(from, peer, true);
            // Over the per-peer bound the request still reaches its ICE agent
            // (which may answer it), but the address is not remembered.
            return Some(peer);
        }
        self.addresses.get(&from).copied()
    }

    /// Records that `peer` uses `address`. Only an `authoritative` (credential-
    /// checked) sighting moves an address from a previous owner.
    pub fn learn(&mut self, address: SocketAddr, peer: PeerId, authoritative: bool) {
        let owner = self.addresses.get(&address).copied();
        if owner == Some(peer) || (owner.is_some() && !authoritative) {
            return;
        }
        let routes = self.peers.entry(peer).or_default();
        if routes.addresses.len() >= MAX_ADDRESSES_PER_PEER {
            if !authoritative {
                return;
            }
            // A credential-checked path (a network change, an ICE restart)
            // replaces this peer's oldest address instead of being ignored.
            let oldest = routes.addresses.remove(0);
            if self.addresses.get(&oldest) == Some(&peer) {
                self.addresses.remove(&oldest);
            }
        }
        let routes = self.peers.entry(peer).or_default();
        routes.addresses.push(address);
        self.addresses.insert(address, peer);
        if let Some(owner) = owner
            && let Some(previous) = self.peers.get_mut(&owner)
        {
            previous.addresses.retain(|a| *a != address);
        }
    }

    /// Removes every route of a closed peer.
    pub fn forget(&mut self, peer: PeerId) {
        let Some(routes) = self.peers.remove(&peer) else {
            return;
        };
        for address in routes.addresses {
            if self.addresses.get(&address) == Some(&peer) {
                self.addresses.remove(&address);
            }
        }
        if let Some(ufrag) = routes.ufrag
            && self.ufrags.get(&ufrag) == Some(&peer)
        {
            self.ufrags.remove(&ufrag);
        }
    }

    #[cfg(test)]
    fn len(&self) -> (usize, usize, usize) {
        (self.addresses.len(), self.ufrags.len(), self.peers.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(username: &[u8], padding: bool) -> Vec<u8> {
        let mut attributes = Vec::new();
        // An unrelated attribute first, so the walk must skip it.
        attributes.extend_from_slice(&0x8022u16.to_be_bytes());
        attributes.extend_from_slice(&3u16.to_be_bytes());
        attributes.extend_from_slice(b"abc\0");
        attributes.extend_from_slice(&USERNAME.to_be_bytes());
        attributes.extend_from_slice(&(username.len() as u16).to_be_bytes());
        attributes.extend_from_slice(username);
        if padding {
            attributes.resize(attributes.len().div_ceil(4) * 4, 0);
        }
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
        datagram.extend_from_slice(&(attributes.len() as u16).to_be_bytes());
        datagram.extend_from_slice(&MAGIC_COOKIE);
        datagram.extend_from_slice(&[7; 12]);
        datagram.extend_from_slice(&attributes);
        datagram
    }

    #[test]
    fn binding_requests_name_the_local_ufrag_and_everything_else_is_ignored() {
        assert_eq!(
            binding_request_ufrag(&binding(b"srv1:cli9", true)),
            Some("srv1")
        );
        let mut response = binding(b"srv1:cli9", true);
        response[0] = 0x01; // 0x0101 Binding success response
        assert_eq!(binding_request_ufrag(&response), None);
        let mut cookie = binding(b"srv1:cli9", true);
        cookie[4] = 0;
        assert_eq!(binding_request_ufrag(&cookie), None);
        // Truncated attribute values and lengths beyond the datagram never panic.
        let truncated = binding(b"srv1:cli9", false);
        assert_eq!(
            binding_request_ufrag(&truncated[..truncated.len() - 2]),
            None
        );
        assert_eq!(binding_request_ufrag(&binding(b"no-colon", true)), None);
        // DTLS and RTP first bytes are not STUN.
        let mut dtls = binding(b"srv1:cli9", true);
        dtls[0] = 22;
        assert_eq!(binding_request_ufrag(&dtls), None);
        assert_eq!(binding_request_ufrag(&[0x80; 40]), None);
        assert_eq!(binding_request_ufrag(&[]), None);
    }

    #[test]
    fn sdp_ufrag_is_found_with_crlf() {
        assert_eq!(
            sdp_ufrag("v=0\r\na=group:BUNDLE 0\r\na=ice-ufrag:AbCd\r\na=ice-pwd:x\r\n").as_deref(),
            Some("AbCd")
        );
        assert_eq!(sdp_ufrag("v=0\r\n"), None);
    }

    #[test]
    fn routes_follow_credentials_are_bounded_and_forgotten_per_owner() {
        let a = PeerId {
            index: 0,
            generation: 1,
        };
        let b = PeerId {
            index: 1,
            generation: 1,
        };
        let mut demux = Demux::default();
        demux.register(a, "ua".into());
        demux.register(b, "ub".into());
        let client: SocketAddr = "203.0.113.5:4000".parse().unwrap();
        // Unknown address without a valid request: dropped.
        assert_eq!(demux.route(client, &[0x80; 12]), None);
        assert_eq!(demux.route(client, &binding(b"zz:c", true)), None);
        // A request with A's ufrag routes and learns the address for A.
        assert_eq!(demux.route(client, &binding(b"ua:c", true)), Some(a));
        assert_eq!(demux.route(client, &[0x80; 12]), Some(a));
        // B's ICE agent sending there (B's client listed A's address as a
        // candidate) does not take it over; only credentials do.
        demux.learn(client, b, false);
        assert_eq!(demux.route(client, &[0x80; 12]), Some(a));
        // The same port presenting B's credentials moves to B.
        assert_eq!(demux.route(client, &binding(b"ub:c", true)), Some(b));
        assert_eq!(demux.route(client, &[0x80; 12]), Some(b));
        // Learning is bounded per peer.
        for port in 0..MAX_ADDRESSES_PER_PEER as u16 + 4 {
            demux.learn(SocketAddr::from(([198, 51, 100, 1], 5000 + port)), a, false);
        }
        assert_eq!(demux.peers[&a].addresses.len(), MAX_ADDRESSES_PER_PEER);
        let unlearned = SocketAddr::from(([198, 51, 100, 1], 5000 + MAX_ADDRESSES_PER_PEER as u16));
        assert_eq!(demux.route(unlearned, &[0x80; 12]), None);
        // Forgetting A leaves B's address and ufrag alone.
        demux.forget(a);
        assert_eq!(demux.len(), (1, 1, 1));
        assert_eq!(demux.route(client, &[0x80; 12]), Some(b));
        demux.forget(b);
        assert_eq!(demux.len(), (0, 0, 0));
    }

    #[test]
    fn ice_restart_moves_routing_to_the_new_ufrag_and_new_paths_are_learned() {
        let a = PeerId {
            index: 0,
            generation: 1,
        };
        let mut demux = Demux::default();
        demux.register(a, "old".into());
        let current: SocketAddr = "203.0.113.5:4000".parse().unwrap();
        assert_eq!(demux.route(current, &binding(b"old:c", true)), Some(a));
        demux.register(a, "new".into());
        // The current path keeps routing media until the restart completes.
        assert_eq!(demux.route(current, &[0x80; 12]), Some(a));
        // Only the new credentials teach new addresses.
        let moved: SocketAddr = "198.51.100.9:4100".parse().unwrap();
        assert_eq!(demux.route(moved, &binding(b"old:c", true)), None);
        assert_eq!(demux.route(moved, &binding(b"new:c", true)), Some(a));
        assert_eq!(demux.len(), (2, 1, 1));
        // At the bound a credential-checked path evicts the oldest address.
        for port in 0..MAX_ADDRESSES_PER_PEER as u16 {
            demux.learn(SocketAddr::from(([198, 51, 100, 1], 6000 + port)), a, false);
        }
        assert_eq!(demux.peers[&a].addresses.len(), MAX_ADDRESSES_PER_PEER);
        let roaming: SocketAddr = "192.0.2.44:7000".parse().unwrap();
        assert_eq!(demux.route(roaming, &binding(b"new:c", true)), Some(a));
        assert_eq!(demux.route(roaming, &[0x80; 12]), Some(a));
        assert_eq!(demux.route(current, &[0x80; 12]), None, "oldest evicted");
        assert_eq!(demux.peers[&a].addresses.len(), MAX_ADDRESSES_PER_PEER);
        demux.forget(a);
        assert_eq!(demux.len(), (0, 0, 0));
    }
}
