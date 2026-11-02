//! When the browser web client shows a system notification (the Web
//! `Notification` API) in addition to its in-app banner. The server already
//! applies preferences, snoozes and Do Not Disturb; these rules only decide
//! presentation in this tab.
use thiscord_shared::notifications::{Alert, Kind};

/// The browser's `Notification.permission`, or unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Unsupported,
    Default,
    Granted,
    Denied,
}
impl Permission {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("granted") => Self::Granted,
            Some("denied") => Self::Denied,
            Some(_) => Self::Default,
            None => Self::Unsupported,
        }
    }
}

/// Inputs for one alert.
#[derive(Debug, Clone, Copy)]
pub struct Presentation {
    pub permission: Permission,
    /// The user turned system notifications on in Settings → Notifications.
    pub enabled: bool,
    /// The tab is hidden or another window has focus.
    pub away: bool,
    pub do_not_disturb: bool,
    /// A reconnect replayed this alert (still-ringing calls).
    pub replayed: bool,
}
impl Presentation {
    /// Show a system notification only while away from the tab, for alerts
    /// that would show a banner. Replays (reconnects) never notify twice.
    pub fn system(&self, alert: &Alert) -> bool {
        self.permission == Permission::Granted
            && self.enabled
            && self.away
            && !self.do_not_disturb
            && !self.replayed
            && alert.delivery.banner
    }
}

/// Seconds a system notification stays before this tab closes it, matching
/// the in-app banner lifetime.
pub fn lifetime_ms(kind: Kind) -> u32 {
    if kind == Kind::IncomingCall {
        30_000
    } else {
        12_000
    }
}

/// Notification title and body. Message text is never included: alerts carry
/// only a source title, so the system shade shows no private content.
pub fn text(alert: &Alert, kind_label: &str) -> (String, String) {
    let title: String = alert.title.chars().take(120).collect();
    (title, kind_label.to_owned())
}

/// Most system notifications this tab keeps open at once, like banners.
pub const MAX_OPEN: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::notifications::{Delivery, Target};

    fn alert(banner: bool) -> Alert {
        Alert {
            id: "a".into(),
            kind: Kind::DirectMessage,
            target: Target::Updates,
            title: "Ada".into(),
            delivery: Delivery {
                banner,
                ..Delivery::default_for(Kind::DirectMessage)
            },
            actor: None,
            call_id: None,
            version: None,
        }
    }
    fn presentation() -> Presentation {
        Presentation {
            permission: Permission::Granted,
            enabled: true,
            away: true,
            do_not_disturb: false,
            replayed: false,
        }
    }

    #[test]
    fn notifies_only_when_away_permitted_enabled_and_banner() {
        assert!(presentation().system(&alert(true)));
        assert!(!presentation().system(&alert(false)));
        for changed in [
            Presentation {
                away: false,
                ..presentation()
            },
            Presentation {
                enabled: false,
                ..presentation()
            },
            Presentation {
                permission: Permission::Default,
                ..presentation()
            },
            Presentation {
                permission: Permission::Denied,
                ..presentation()
            },
            Presentation {
                permission: Permission::Unsupported,
                ..presentation()
            },
            Presentation {
                do_not_disturb: true,
                ..presentation()
            },
            Presentation {
                replayed: true,
                ..presentation()
            },
        ] {
            assert!(!changed.system(&alert(true)), "{changed:?}");
        }
    }

    #[test]
    fn permission_parsing_and_lifetimes() {
        assert_eq!(Permission::parse(Some("granted")), Permission::Granted);
        assert_eq!(Permission::parse(Some("denied")), Permission::Denied);
        assert_eq!(Permission::parse(Some("default")), Permission::Default);
        assert_eq!(Permission::parse(None), Permission::Unsupported);
        assert_eq!(lifetime_ms(Kind::IncomingCall), 30_000);
        assert_eq!(lifetime_ms(Kind::GuildMessage), 12_000);
        let mut long = alert(true);
        long.title = "x".repeat(500);
        assert_eq!(text(&long, "Direct message").0.len(), 120);
    }
}
