//! In-place ICE restarts for voice, shared by the desktop and `/app` clients:
//! recover a lost media path and refresh time-limited TURN credentials before
//! they expire, without leaving the call. A restart that does not reconnect in
//! time falls back to the full reconnection path. Pure state over a caller's
//! millisecond clock (monotonic on desktop, `Date.now()` in browsers); the
//! voice loops perform the I/O.
use thiscord_shared::voice::IceServer;

/// The path must stay lost this long (after ICE already reported it
/// disconnected) before a restart is requested.
const LOSS_GRACE_MS: u64 = 1_000;
/// The server must answer a restart request within this time.
const OFFER_TIMEOUT_MS: u64 = 10_000;
/// After answering, the path must reconnect within this time.
const CONNECT_TIMEOUT_MS: u64 = 10_000;
/// A path connected this long after the answer counts as restarted.
const SETTLE_MS: u64 = 1_000;
/// While a direct path is in use, check again this often whether credentials
/// matter (the path may move to a relay).
const RELAY_RECHECK_MS: u64 = 30_000;
/// A path lost again this soon after a restart settled did not really recover:
/// reconnect in full instead of restarting in a loop.
const HOLD_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    PathLost,
    NetworkChanged,
    RelayRefresh,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Send `ClientEvent::IceRestart`.
    Request(Reason),
    /// The restart did not recover the path: reconnect in full.
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Requested(u64),
    Answered(u64),
}

pub struct IceRecovery {
    negotiated: bool,
    phase: Phase,
    lost_since: Option<u64>,
    settled_at: Option<u64>,
    refresh_at: Option<u64>,
    expires_at: Option<u64>,
}

/// Refresh margin before expiry: five minutes, or a fifth of a short lifetime.
fn margin_ms(lifetime_ms: u64) -> u64 {
    (lifetime_ms / 5).min(300_000)
}

impl IceRecovery {
    /// `negotiated`: the server accepted `ice_restart` for this session.
    pub fn new(negotiated: bool, servers: &[IceServer], now: u64) -> Self {
        let mut recovery = Self {
            negotiated,
            phase: Phase::Idle,
            lost_since: None,
            settled_at: None,
            refresh_at: None,
            expires_at: None,
        };
        recovery.credentials(servers, now);
        recovery
    }

    /// Schedules the refresh of freshly received relay credentials.
    pub fn credentials(&mut self, servers: &[IceServer], now: u64) {
        let lifetime = servers
            .iter()
            .filter_map(|server| server.expires_in_secs)
            .min()
            .map(|secs| secs.saturating_mul(1_000));
        self.expires_at = lifetime.map(|lifetime| now.saturating_add(lifetime));
        self.refresh_at =
            lifetime.map(|lifetime| now.saturating_add(lifetime - margin_ms(lifetime)));
    }

    /// Whether the call is recovering its path (shown as "Reconnecting…").
    pub fn recovering(&self) -> bool {
        self.phase != Phase::Idle || self.lost_since.is_some()
    }

    pub fn tick(&mut self, connected: bool, can_signal: bool, now: u64) -> Action {
        match self.phase {
            Phase::Requested(at) if now.saturating_sub(at) > OFFER_TIMEOUT_MS => Action::Reconnect,
            Phase::Requested(_) => Action::Wait,
            Phase::Answered(at) => {
                let elapsed = now.saturating_sub(at);
                if connected && elapsed >= SETTLE_MS {
                    self.phase = Phase::Idle;
                    self.lost_since = None;
                    self.settled_at = Some(now);
                    Action::Wait
                } else if !connected && elapsed > CONNECT_TIMEOUT_MS {
                    Action::Reconnect
                } else {
                    Action::Wait
                }
            }
            Phase::Idle if connected => {
                self.lost_since = None;
                Action::Wait
            }
            Phase::Idle => {
                let lost = *self.lost_since.get_or_insert(now);
                if !self.negotiated || !can_signal || now.saturating_sub(lost) < LOSS_GRACE_MS {
                    Action::Wait
                } else if self
                    .settled_at
                    .is_some_and(|settled| lost.saturating_sub(settled) < HOLD_MS)
                {
                    Action::Reconnect
                } else {
                    self.phase = Phase::Requested(now);
                    Action::Request(Reason::PathLost)
                }
            }
        }
    }

    /// The device's network changed (it came back online, or its connection
    /// type changed): restart now instead of waiting for ICE to notice.
    pub fn network_changed(&mut self, can_signal: bool, now: u64) -> Action {
        if self.negotiated && can_signal && self.phase == Phase::Idle {
            self.phase = Phase::Requested(now);
            Action::Request(Reason::NetworkChanged)
        } else {
            Action::Wait
        }
    }

    /// Credentials expire soon: check whether the selected path uses a relay.
    pub fn refresh_due(&self, now: u64) -> bool {
        self.negotiated
            && self.phase == Phase::Idle
            && self.refresh_at.is_some_and(|at| now >= at)
            && self.expires_at.is_some_and(|at| now < at)
    }

    /// The selected path's local candidate type at refresh time: a relayed or
    /// unknown path restarts with fresh credentials, a direct one checks again.
    pub fn relay_checked(&mut self, relayed: Option<bool>, now: u64) -> Action {
        if relayed == Some(false) {
            let next = now.saturating_add(RELAY_RECHECK_MS);
            self.refresh_at = self.expires_at.filter(|at| next < *at).map(|_| next);
            return Action::Wait;
        }
        self.refresh_at = None;
        self.phase = Phase::Requested(now);
        Action::Request(Reason::RelayRefresh)
    }

    /// The restart answer was sent; the new relay credentials are scheduled.
    pub fn answered(&mut self, servers: &[IceServer], now: u64) {
        self.phase = Phase::Answered(now);
        self.credentials(servers, now);
    }

    /// Whether an offer from the server is expected now.
    pub fn awaiting_offer(&self) -> bool {
        matches!(self.phase, Phase::Requested(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(expires_in_secs: u64) -> IceServer {
        IceServer {
            urls: vec!["turn:turn.example.test:3478?transport=udp".into()],
            username: "1:account".into(),
            credential: "credential".into(),
            expires_in_secs: Some(expires_in_secs),
        }
    }

    #[test]
    fn lost_paths_restart_once_and_fall_back_to_a_full_reconnect() {
        let mut ice = IceRecovery::new(true, &[], 0);
        assert_eq!(ice.tick(true, true, 0), Action::Wait);
        assert!(!ice.recovering());
        // Brief loss: wait for the grace period, and never while signaling is down.
        assert_eq!(ice.tick(false, true, 100), Action::Wait);
        assert!(ice.recovering());
        assert_eq!(ice.tick(false, false, 1_500), Action::Wait);
        assert_eq!(
            ice.tick(false, true, 1_600),
            Action::Request(Reason::PathLost)
        );
        assert!(ice.awaiting_offer());
        // One restart in flight: no second request.
        assert_eq!(ice.tick(false, true, 3_000), Action::Wait);
        assert_eq!(ice.network_changed(true, 3_000), Action::Wait);
        ice.answered(&[], 3_500);
        assert_eq!(ice.tick(true, true, 4_000), Action::Wait);
        assert!(ice.recovering(), "settling");
        assert_eq!(ice.tick(true, true, 4_600), Action::Wait);
        assert!(!ice.recovering(), "restarted in place");
        // A restart that does not hold is not retried in a loop.
        let mut relapse = IceRecovery::new(true, &[], 0);
        relapse.tick(false, true, 0);
        relapse.tick(false, true, 1_000);
        relapse.answered(&[], 2_000);
        relapse.tick(true, true, 3_000);
        relapse.tick(false, true, 10_000);
        assert_eq!(relapse.tick(false, true, 11_000), Action::Reconnect);
        // Much later, a new loss restarts in place again; one that never
        // reconnects ends in a full reconnection.
        assert_eq!(ice.tick(false, true, 40_000), Action::Wait, "grace again");
        assert_eq!(
            ice.tick(false, true, 41_100),
            Action::Request(Reason::PathLost)
        );
        ice.answered(&[], 41_200);
        assert_eq!(ice.tick(false, true, 51_300), Action::Reconnect);
        // And so does an unanswered request.
        let mut unanswered = IceRecovery::new(true, &[], 0);
        unanswered.tick(false, true, 0);
        assert_eq!(
            unanswered.tick(false, true, 1_000),
            Action::Request(Reason::PathLost)
        );
        assert_eq!(unanswered.tick(false, true, 11_001), Action::Reconnect);
        // Older servers cannot restart: the existing failure path applies.
        let mut legacy = IceRecovery::new(false, &[turn(3600)], 0);
        legacy.tick(false, true, 0);
        assert_eq!(legacy.tick(false, true, 60_000), Action::Wait);
        assert_eq!(legacy.network_changed(true, 60_000), Action::Wait);
        assert!(!legacy.refresh_due(3_600_000));
    }

    #[test]
    fn network_changes_restart_immediately_when_idle() {
        let mut ice = IceRecovery::new(true, &[], 0);
        assert_eq!(ice.network_changed(false, 10), Action::Wait, "no signaling");
        assert_eq!(
            ice.network_changed(true, 20),
            Action::Request(Reason::NetworkChanged)
        );
        assert!(ice.awaiting_offer());
        assert_eq!(ice.tick(true, true, 10_021), Action::Reconnect);
    }

    #[test]
    fn relay_credentials_refresh_before_expiry_only_when_relayed() {
        let mut ice = IceRecovery::new(true, &[turn(3600)], 0);
        // Five minutes of margin for an hour.
        assert!(!ice.refresh_due(3_299_999));
        assert!(ice.refresh_due(3_300_000));
        // A direct path does not need the relay: check again later.
        let now = 3_300_000;
        assert_eq!(ice.relay_checked(Some(false), now), Action::Wait);
        assert!(!ice.refresh_due(now + 29_999));
        assert!(ice.refresh_due(now + 30_000));
        // A relayed (or unknown) path restarts with fresh credentials.
        let now = now + 30_000;
        assert_eq!(
            ice.relay_checked(None, now),
            Action::Request(Reason::RelayRefresh)
        );
        assert!(!ice.refresh_due(now), "one restart at a time");
        let answered = now + 1_000;
        ice.answered(&[turn(3600)], answered);
        assert!(!ice.refresh_due(answered + 3_300_000), "not settled");
        // Once the restarted path settles, the new credentials are scheduled.
        ice.tick(true, true, answered + 2_000);
        assert!(!ice.refresh_due(answered + 3_299_999));
        assert!(ice.refresh_due(answered + 3_300_000));
        // Short test lifetimes keep a fifth as margin.
        let short = IceRecovery::new(true, &[turn(60)], 0);
        assert!(!short.refresh_due(47_999));
        assert!(short.refresh_due(48_000));
        // No relay credentials, nothing to refresh; expired ones are not retried.
        assert!(!IceRecovery::new(true, &[], 0).refresh_due(86_400_000));
        let mut late = IceRecovery::new(true, &[turn(60)], 0);
        assert_eq!(late.relay_checked(Some(false), 48_000), Action::Wait);
        assert!(!late.refresh_due(78_000));
    }
}
