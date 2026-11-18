//! Per-process connection admission limits. Connections beyond a limit are
//! refused with the shared `rate_limited` error before any database work, so
//! admitted users keep their capacity. See docs/load-benchmarks.md for the
//! measurements behind the defaults.
use std::{
    collections::HashMap,
    hash::Hash,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
};

use axum::http::HeaderMap;
use thiscord_shared::AccountId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::BoxError;

pub const SOCKETS_VAR: &str = "THISCORD_MAX_SOCKETS";
pub const VOICE_VAR: &str = "THISCORD_MAX_VOICE_CONNECTIONS";
/// Each of the chat, friends and notification socket types admits this many.
/// A signed-in desktop or browser client holds one of each.
pub const DEFAULT_SOCKETS: usize = 512;
/// Concurrent voice/video connections (SFU peers), including test bots.
pub const DEFAULT_VOICE: usize = 96;
const SOCKET_RANGE: std::ops::RangeInclusive<usize> = 16..=4096;
const VOICE_RANGE: std::ops::RangeInclusive<usize> = 2..=1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub sockets: usize,
    pub voice: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            sockets: DEFAULT_SOCKETS,
            voice: DEFAULT_VOICE,
        }
    }
}
impl Limits {
    /// Parses the optional variables; any other value fails startup.
    pub fn parse(sockets: Option<&str>, voice: Option<&str>) -> Result<Self, BoxError> {
        Ok(Self {
            sockets: bounded(SOCKETS_VAR, sockets, DEFAULT_SOCKETS, SOCKET_RANGE)?,
            voice: bounded(VOICE_VAR, voice, DEFAULT_VOICE, VOICE_RANGE)?,
        })
    }
    pub fn from_env() -> Result<Self, BoxError> {
        let read = |name: &str| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("{name} must contain valid Unicode")),
        };
        Self::parse(read(SOCKETS_VAR)?.as_deref(), read(VOICE_VAR)?.as_deref())
    }
}
fn bounded(
    name: &str,
    value: Option<&str>,
    default: usize,
    range: std::ops::RangeInclusive<usize>,
) -> Result<usize, BoxError> {
    let Some(value) = value else {
        return Ok(default);
    };
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| range.contains(n))
        .ok_or_else(|| {
            format!(
                "{name} must be an integer from {} to {}",
                range.start(),
                range.end()
            )
            .into()
        })
}

/// Admission caps for one real-time endpoint besides its main limit. Pending
/// counts only upgrades still waiting to authenticate (normally a round trip
/// plus one database check). See docs/load-benchmarks.md#admission-limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Caps {
    /// Unauthenticated upgrades in flight, across all addresses.
    pub pending: usize,
    /// Unauthenticated upgrades in flight from one client address.
    pub per_address: usize,
    /// Authenticated connections one account holds at once.
    pub per_account: usize,
}
impl Caps {
    /// Chat, friends and notification sockets (each type separately).
    pub(crate) const SOCKETS: Self = Self {
        pending: 256,
        per_address: 32,
        per_account: 16,
    };
    /// Voice signaling: joins and signaling resumes.
    pub(crate) const VOICE: Self = Self {
        pending: 64,
        per_address: 16,
        per_account: 8,
    };
    /// Scripting-bot sockets.
    pub(crate) const BOTS: Self = Self {
        pending: 32,
        per_address: 16,
        per_account: 8,
    };
}

/// One endpoint's admission. An upgrade first takes a pending slot (a small
/// budget, also capped per client address); only after its first frame has
/// authenticated does it take a main slot, capped per account. Everything is in
/// memory, checked before database work and released on drop. The maps keep
/// only nonzero counts, so the semaphores bound their size.
#[derive(Clone)]
pub(crate) struct Admission(Arc<Shared>);
struct Shared {
    main: Arc<Semaphore>,
    pending: Arc<Semaphore>,
    caps: Caps,
    addresses: Mutex<HashMap<IpAddr, usize>>,
    accounts: Mutex<HashMap<AccountId, usize>>,
}
impl Admission {
    pub(crate) fn new(main: usize, caps: Caps) -> Self {
        assert!(main > 0 && caps.pending > 0 && caps.per_address > 0 && caps.per_account > 0);
        Self(Arc::new(Shared {
            main: Arc::new(Semaphore::new(main)),
            pending: Arc::new(Semaphore::new(caps.pending)),
            caps,
            addresses: Mutex::default(),
            accounts: Mutex::default(),
        }))
    }
    /// Admits an unauthenticated upgrade from `peer`, keyed by
    /// `client_ip::limit_address`. Without a peer address (only test and
    /// benchmark servers omit it) all upgrades share one key. Refuses while
    /// every main slot is taken, so a full server still answers the upgrade
    /// itself with 429 instead of after authentication.
    pub(crate) fn pending(&self, peer: Option<SocketAddr>, headers: &HeaderMap) -> Option<Ticket> {
        let address = peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |peer| {
            crate::client_ip::limit_address(peer, headers)
        });
        self.pending_from(address)
    }
    fn pending_from(&self, address: IpAddr) -> Option<Ticket> {
        if self.0.main.available_permits() == 0 {
            return None;
        }
        let permit = self.0.pending.clone().try_acquire_owned().ok()?;
        if !increment(&self.0.addresses, address, self.0.caps.per_address) {
            return None;
        }
        Some(Ticket {
            admission: self.clone(),
            state: State::Pending {
                _permit: permit,
                address,
            },
        })
    }
}

/// Counts `key` if it is below `cap`.
fn increment<K: Eq + Hash>(map: &Mutex<HashMap<K, usize>>, key: K, cap: usize) -> bool {
    let mut map = map.lock().unwrap_or_else(|p| p.into_inner());
    let count = map.entry(key).or_default();
    // A refused key already has an entry: every cap is at least one.
    if *count >= cap {
        return false;
    }
    *count += 1;
    true
}
fn decrement<K: Eq + Hash>(map: &Mutex<HashMap<K, usize>>, key: &K) {
    let mut map = map.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(count) = map.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            map.remove(key);
        }
    }
}

/// A connection's admission, held for the connection's lifetime.
pub(crate) struct Ticket {
    admission: Admission,
    state: State,
}
enum State {
    Pending {
        _permit: OwnedSemaphorePermit,
        address: IpAddr,
    },
    Admitted {
        _permit: OwnedSemaphorePermit,
        account: AccountId,
    },
    Released,
}
impl Ticket {
    /// Moves an authenticated connection from the pending budget to a main
    /// slot counted against `account`. Refused (main slots full or the account
    /// at its cap), the ticket stays pending and the caller closes the
    /// connection with `rate_limited`. An admitted ticket stays admitted.
    pub(crate) fn promote(&mut self, account: AccountId) -> bool {
        if !matches!(self.state, State::Pending { .. }) {
            return matches!(self.state, State::Admitted { .. });
        }
        let shared = &self.admission.0;
        let Ok(permit) = shared.main.clone().try_acquire_owned() else {
            return false;
        };
        if !increment(&shared.accounts, account, shared.caps.per_account) {
            return false;
        }
        self.release();
        self.state = State::Admitted {
            _permit: permit,
            account,
        };
        true
    }
    /// Gives the slot back before the connection ends.
    pub(crate) fn release(&mut self) {
        let shared = &self.admission.0;
        match std::mem::replace(&mut self.state, State::Released) {
            State::Pending { address, .. } => decrement(&shared.addresses, &address),
            State::Admitted { account, .. } => decrement(&shared.accounts, &account),
            State::Released => {}
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_default_and_reject_out_of_range_values() {
        assert_eq!(Limits::parse(None, None).unwrap(), Limits::default());
        assert_eq!(
            Limits::parse(Some(" 1024 "), Some("200")).unwrap(),
            Limits {
                sockets: 1024,
                voice: 200
            }
        );
        for invalid in ["0", "15", "4097", "-1", "", "many", "1.5"] {
            assert!(Limits::parse(Some(invalid), None).is_err(), "{invalid}");
        }
        for invalid in ["0", "1", "1025", "x"] {
            assert!(Limits::parse(None, Some(invalid)).is_err(), "{invalid}");
        }
    }

    const CAPS: Caps = Caps {
        pending: 3,
        per_address: 2,
        per_account: 2,
    };
    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }
    fn account() -> AccountId {
        AccountId::from_uuid(uuid::Uuid::new_v4())
    }
    fn counts(admission: &Admission) -> (usize, usize) {
        let addresses = admission.0.addresses.lock().unwrap().len();
        let accounts = admission.0.accounts.lock().unwrap().len();
        (addresses, accounts)
    }

    #[test]
    fn pending_slots_are_capped_per_address_and_overall() {
        let admission = Admission::new(4, CAPS);
        let a1 = admission.pending_from(ip(1)).unwrap();
        let _a2 = admission.pending_from(ip(1)).unwrap();
        assert!(admission.pending_from(ip(1)).is_none(), "per-address cap");
        let _b1 = admission.pending_from(ip(2)).unwrap();
        assert!(admission.pending_from(ip(3)).is_none(), "overall cap");
        // Unauthenticated connections never touch the main slots.
        assert_eq!(admission.0.main.available_permits(), 4);
        drop(a1);
        assert!(admission.pending_from(ip(3)).is_some());
        assert!(admission.pending_from(ip(1)).is_some());
    }

    #[test]
    fn tickets_release_on_drop_and_the_maps_stay_bounded() {
        let admission = Admission::new(4, CAPS);
        for round in 0..100u8 {
            let mut ticket = admission.pending_from(ip(round)).unwrap();
            assert_eq!(counts(&admission), (1, 0));
            if round % 2 == 0 {
                assert!(ticket.promote(account()));
                assert_eq!(counts(&admission), (0, 1));
            }
        }
        assert_eq!(counts(&admission), (0, 0));
        assert_eq!(admission.0.main.available_permits(), 4);
        assert_eq!(admission.0.pending.available_permits(), CAPS.pending);
        // A refused address leaves no entry behind either.
        let held: Vec<_> = (0..2)
            .map(|_| admission.pending_from(ip(9)).unwrap())
            .collect();
        assert!(admission.pending_from(ip(9)).is_none());
        drop(held);
        assert_eq!(counts(&admission), (0, 0));
    }

    #[test]
    fn promotion_moves_to_main_slots_capped_per_account() {
        let admission = Admission::new(3, CAPS);
        let (one, two) = (account(), account());
        let mut first = admission.pending_from(ip(1)).unwrap();
        assert!(first.promote(one));
        assert!(first.promote(one), "promoting again keeps the slot");
        // The pending slot was returned: the address may open two more.
        let mut second = admission.pending_from(ip(1)).unwrap();
        let mut third = admission.pending_from(ip(1)).unwrap();
        assert!(second.promote(one));
        assert!(!third.promote(one), "per-account cap");
        assert_eq!(admission.0.main.available_permits(), 1);
        // Refused, the ticket stays pending and another account is unaffected.
        assert!(third.promote(two));
        assert_eq!(admission.0.main.available_permits(), 0);
        // Main slots full: new upgrades are refused before authentication.
        assert!(admission.pending_from(ip(2)).is_none());
        drop(second);
        let mut fourth = admission.pending_from(ip(2)).unwrap();
        let mut late = admission.pending_from(ip(3)).unwrap();
        assert!(fourth.promote(two));
        // Both passed the upgrade check; the slower one loses the last slot.
        assert!(!late.promote(account()));
        drop((first, third, fourth, late));
        assert_eq!(counts(&admission), (0, 0));
        assert_eq!(admission.0.main.available_permits(), 3);
        assert_eq!(admission.0.pending.available_permits(), CAPS.pending);
    }

    #[test]
    fn release_returns_the_slot_early() {
        let admission = Admission::new(2, CAPS);
        let mut ticket = admission.pending_from(ip(1)).unwrap();
        ticket.release();
        assert_eq!(counts(&admission), (0, 0));
        assert!(
            !ticket.promote(account()),
            "a released ticket is not admitted"
        );
        let mut admitted = admission.pending_from(ip(1)).unwrap();
        assert!(admitted.promote(account()));
        admitted.release();
        assert_eq!(admission.0.main.available_permits(), 2);
        assert_eq!(counts(&admission), (0, 0));
    }
}
