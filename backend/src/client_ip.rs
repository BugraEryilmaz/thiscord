//! Client addresses for per-address rate limits. `CF-Connecting-IP` is honoured
//! only from a peer inside `TRUSTED_PROXY_CIDRS`; every other header, and any
//! header from an untrusted peer, is ignored. See docs/accounts.md.
use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::OnceLock,
};

use axum::http::HeaderMap;

use crate::BoxError;

pub const TRUSTED_PROXIES_VAR: &str = "TRUSTED_PROXY_CIDRS";
const CLIENT_HEADER: &str = "cf-connecting-ip";

static TRUSTED: OnceLock<Vec<Cidr>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn contains(self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                mask(u32::from(network).into(), self.prefix, 32)
                    == mask(u32::from(address).into(), self.prefix, 32)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                mask(network.into(), self.prefix, 128) == mask(address.into(), self.prefix, 128)
            }
            _ => false,
        }
    }
}

/// Keeps the top `prefix` of `bits` bits.
fn mask(value: u128, prefix: u8, bits: u8) -> u128 {
    if prefix == 0 {
        return 0;
    }
    value & (u128::MAX << (bits - prefix)) & (u128::MAX >> (128 - u32::from(bits)))
}

/// Reads `TRUSTED_PROXY_CIDRS` once at startup. Unset or empty trusts no proxy.
pub fn configure() -> Result<(), BoxError> {
    let value = match std::env::var(TRUSTED_PROXIES_VAR) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(_) => return Err(format!("{TRUSTED_PROXIES_VAR} must contain valid Unicode").into()),
    };
    let list = parse(&value)?;
    if !list.is_empty() {
        tracing::info!(
            ranges = list.len(),
            "trusting CF-Connecting-IP from configured proxies"
        );
    }
    TRUSTED
        .set(list)
        .map_err(|_| format!("{TRUSTED_PROXIES_VAR} is already configured").into())
}

fn parse(value: &str) -> Result<Vec<Cidr>, BoxError> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            parse_cidr(entry).ok_or_else(|| {
                format!(
                    "{TRUSTED_PROXIES_VAR} must be comma-separated CIDRs such as 173.245.48.0/20,2400:cb00::/32 \
                     (no host bits, no /0, IPv4 ranges in IPv4 form); invalid entry: {entry}"
                )
                .into()
            })
        })
        .collect()
}

fn parse_cidr(entry: &str) -> Option<Cidr> {
    let (address, prefix) = match entry.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (entry, None),
    };
    let network: IpAddr = address.parse().ok()?;
    // Peers are matched in canonical form, so a mapped range would never match.
    if network.to_canonical() != network {
        return None;
    }
    let bits = if network.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        // Digits only: no sign, whitespace or leading `+`.
        Some(prefix) if prefix.bytes().all(|b| b.is_ascii_digit()) => prefix.parse().ok()?,
        Some(_) => return None,
        None => bits,
    };
    let cidr = Cidr { network, prefix };
    // `/0` would trust every peer; host bits usually mean a typo.
    let value = match network {
        IpAddr::V4(v4) => u32::from(v4).into(),
        IpAddr::V6(v6) => u128::from(v6),
    };
    (prefix > 0 && prefix <= bits && mask(value, prefix, bits) == value).then_some(cidr)
}

/// The address a per-client limit counts against: canonical IPv4, or the
/// client's IPv6 /64 (one end site rotating within its prefix stays one key).
/// Usable from any handler holding `ConnectInfo<SocketAddr>` and the headers.
pub(crate) fn limit_address(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    resolve(
        peer.ip(),
        headers,
        TRUSTED.get().map_or(&[][..], Vec::as_slice),
    )
}

/// Database limiter key fragment; `local` when no peer is attached (tests).
pub(crate) fn limit_key(peer: Option<SocketAddr>, headers: &HeaderMap) -> String {
    peer.map_or_else(
        || "local".into(),
        |peer| limit_address(peer, headers).to_string(),
    )
}

fn resolve(peer: IpAddr, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    let peer = peer.to_canonical();
    let client = if trusted.iter().any(|cidr| cidr.contains(peer)) {
        // A missing, repeated or malformed header falls back to the proxy's
        // shared budget rather than keying on attacker-chosen text.
        let mut values = headers.get_all(CLIENT_HEADER).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
                .map_or(peer, |client| client.to_canonical()),
            _ => peer,
        }
    } else {
        peer
    };
    bucket(client)
}

fn bucket(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(CLIENT_HEADER, value.parse().unwrap());
        }
        map
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn cloudflare() -> Vec<Cidr> {
        parse("173.245.48.0/20, 2400:cb00::/32").unwrap()
    }

    #[test]
    fn untrusted_peer_headers_are_ignored() {
        let spoof = {
            let mut map = headers(&["203.0.113.9"]);
            map.insert("x-forwarded-for", "198.51.100.1".parse().unwrap());
            map
        };
        assert_eq!(resolve(ip("192.0.2.7"), &spoof, &[]), ip("192.0.2.7"));
        assert_eq!(
            resolve(ip("192.0.2.7"), &spoof, &cloudflare()),
            ip("192.0.2.7")
        );
    }

    #[test]
    fn trusted_peer_header_is_honoured() {
        let trusted = cloudflare();
        assert_eq!(
            resolve(ip("173.245.48.10"), &headers(&["203.0.113.9"]), &trusted),
            ip("203.0.113.9")
        );
        assert_eq!(
            resolve(
                ip("2400:cb00:1::5"),
                &headers(&[" 2001:db8:a:b:c:d:e:f "]),
                &trusted
            ),
            ip("2001:db8:a:b::")
        );
        // X-Forwarded-For is never used, even from a trusted peer.
        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(
            resolve(ip("173.245.48.10"), &forwarded, &trusted),
            ip("173.245.48.10")
        );
    }

    #[test]
    fn malformed_or_missing_header_from_trusted_peer_uses_peer() {
        let trusted = cloudflare();
        let proxy = ip("173.245.48.10");
        for bad in [
            &["not-an-ip"][..],
            &["203.0.113.9, 198.51.100.1"],
            &["203.0.113.9:443"],
            &[""],
            &["203.0.113.9", "198.51.100.1"],
            &[],
        ] {
            assert_eq!(resolve(proxy, &headers(bad), &trusted), proxy, "{bad:?}");
        }
        let mut opaque = HeaderMap::new();
        opaque.insert(
            CLIENT_HEADER,
            axum::http::HeaderValue::from_bytes(b"\xff1.2.3.4").unwrap(),
        );
        assert_eq!(resolve(proxy, &opaque, &trusted), proxy);
        // IPv6 proxies fall back to their own /64, still a shared bucket.
        assert_eq!(
            resolve(ip("2400:cb00:1:2:3::5"), &HeaderMap::new(), &trusted),
            ip("2400:cb00:1:2::")
        );
    }

    #[test]
    fn ipv4_mapped_addresses_are_ipv4() {
        let trusted = cloudflare();
        // A dual-stack listener reports IPv4 peers as mapped IPv6.
        assert_eq!(
            resolve(ip("::ffff:192.0.2.7"), &HeaderMap::new(), &[]),
            ip("192.0.2.7")
        );
        assert_eq!(
            resolve(
                ip("::ffff:173.245.48.10"),
                &headers(&["::ffff:203.0.113.9"]),
                &trusted
            ),
            ip("203.0.113.9")
        );
        assert_eq!(
            limit_key(
                Some("[::ffff:192.0.2.7]:443".parse().unwrap()),
                &HeaderMap::new()
            ),
            "192.0.2.7"
        );
        assert_eq!(limit_key(None, &HeaderMap::new()), "local");
    }

    #[test]
    fn ipv6_is_bucketed_by_64() {
        let a = resolve(ip("2001:db8:1:2:aaaa::1"), &HeaderMap::new(), &[]);
        let b = resolve(ip("2001:db8:1:2:ffff:1:2:3"), &HeaderMap::new(), &[]);
        let other = resolve(ip("2001:db8:1:3::1"), &HeaderMap::new(), &[]);
        assert_eq!(a, b);
        assert_eq!(a, ip("2001:db8:1:2::"));
        assert_ne!(a, other);
        assert_eq!(
            resolve(ip("192.0.2.7"), &HeaderMap::new(), &[]),
            ip("192.0.2.7")
        );
    }

    #[test]
    fn configuration_parses_and_rejects() {
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(parse(" , ").unwrap(), vec![]);
        let list = parse("173.245.48.0/20,\n 2400:cb00::/32 ,127.0.0.1").unwrap();
        assert_eq!(
            list,
            vec![
                Cidr {
                    network: ip("173.245.48.0"),
                    prefix: 20
                },
                Cidr {
                    network: ip("2400:cb00::"),
                    prefix: 32
                },
                Cidr {
                    network: ip("127.0.0.1"),
                    prefix: 32
                },
            ]
        );
        assert!(list[0].contains(ip("173.245.63.255")));
        assert!(!list[0].contains(ip("173.245.64.0")));
        assert!(list[1].contains(ip("2400:cb00:ffff::1")));
        assert!(!list[1].contains(ip("2400:cb01::1")));
        assert!(!list[0].contains(ip("::ffff:173.245.48.1")));
        for bad in [
            "173.245.48.0/33",
            "2400:cb00::/129",
            "173.245.48.1/20",
            "0.0.0.0/0",
            "::/0",
            "::ffff:173.245.48.0/116",
            "173.245.48.0/+20",
            "173.245.48.0/",
            "example.com/24",
            "173.245.48.0/20;2400:cb00::/32",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("1.2.3.4/32").unwrap()[0].contains(ip("1.2.3.4")));
        assert!(parse("2001:db8::1/128").unwrap()[0].contains(ip("2001:db8::1")));
    }
}
