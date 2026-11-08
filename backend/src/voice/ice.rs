use base64::Engine;
use hmac::{Hmac, Mac};
use thiscord_shared::{AccountId, voice::IceServer};
/// Lifetime of generated TURN REST credentials. Clients refresh them with an
/// ICE restart shortly before expiry, so long relayed calls continue.
pub(super) const TURN_TTL_VAR: &str = "THISCORD_TURN_TTL_SECS";
const DEFAULT_TURN_TTL: u64 = 3600;

/// `THISCORD_TURN_TTL_SECS`: 60 to 86400 seconds, default one hour. A short
/// value is useful to exercise refreshes; production keeps the default.
fn turn_ttl(value: Option<&str>) -> Result<u64, String> {
    match value.map(str::trim) {
        None => Ok(DEFAULT_TURN_TTL),
        Some(value) => value
            .parse()
            .ok()
            .filter(|ttl| (60..=86_400).contains(ttl))
            .ok_or_else(|| format!("{TURN_TTL_VAR} must be 60 to 86400 seconds")),
    }
}

/// Startup check of the relay credential lifetime.
#[allow(
    dead_code,
    reason = "examples/turn_probe.rs includes this file without the server"
)]
pub(super) fn validate() -> Result<(), String> {
    turn_ttl(std::env::var(TURN_TTL_VAR).ok().as_deref()).map(|_| ())
}

/// The relay settings read from the environment.
struct Settings {
    stun: Option<String>,
    turn: Option<String>,
    secret: Option<String>,
    ttl: Option<String>,
}

pub(super) fn servers(account: AccountId) -> Result<Vec<IceServer>, String> {
    let settings = Settings {
        stun: std::env::var("THISCORD_STUN_URL").ok(),
        turn: std::env::var("THISCORD_TURN_URL").ok(),
        secret: std::env::var("THISCORD_TURN_SECRET").ok(),
        ttl: std::env::var(TURN_TTL_VAR).ok(),
    };
    servers_from(&settings, account, chrono::Utc::now().timestamp())
}

/// Account-bound TURN REST credentials (`expiry:account`, HMAC-SHA1) valid
/// from `now` for the configured lifetime. Every call mints fresh ones.
fn servers_from(
    settings: &Settings,
    account: AccountId,
    now: i64,
) -> Result<Vec<IceServer>, String> {
    let mut servers = vec![];
    if let Some(url) = settings.stun.clone() {
        if !url.starts_with("stun:") {
            return Err("Invalid STUN configuration".into());
        }
        servers.push(IceServer {
            urls: vec![url],
            username: String::new(),
            credential: String::new(),
            expires_in_secs: None,
        });
    }
    if let Some(url) = settings.turn.clone() {
        if !url.starts_with("turn:") && !url.starts_with("turns:") {
            return Err("Invalid TURN configuration".into());
        }
        let secret = settings.secret.as_deref().ok_or("TURN secret missing")?;
        if secret.len() < 32 {
            return Err("TURN secret must contain at least 32 bytes".into());
        }
        let ttl = turn_ttl(settings.ttl.as_deref())?;
        let username = format!("{}:{account}", now + ttl as i64);
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes())
            .map_err(|_| "Invalid TURN secret")?;
        mac.update(username.as_bytes());
        let credential =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        servers.push(IceServer {
            urls: vec![url],
            username,
            credential,
            expires_in_secs: Some(ttl),
        });
    }
    Ok(servers)
}
pub(super) fn configuration(servers: &[IceServer]) -> webrtc::peer_connection::RTCConfiguration {
    webrtc::peer_connection::RTCConfigurationBuilder::new()
        .with_ice_servers(
            servers
                .iter()
                .map(|s| webrtc::peer_connection::RTCIceServer {
                    urls: s.urls.clone(),
                    username: s.username.clone(),
                    credential: s.credential.clone(),
                })
                .collect(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_lifetime_is_bounded_and_defaults_to_an_hour() {
        assert_eq!(turn_ttl(None), Ok(3600));
        assert_eq!(turn_ttl(Some(" 120 ")), Ok(120));
        for invalid in ["59", "86401", "-1", "soon", ""] {
            assert!(turn_ttl(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn refreshed_relay_credentials_carry_their_lifetime_and_a_later_expiry() {
        let account = AccountId::from_uuid(uuid::Uuid::new_v4());
        let settings = Settings {
            stun: Some("stun:turn.example.test:3478".into()),
            turn: Some("turn:turn.example.test:3478?transport=udp".into()),
            secret: Some("test-only-secret-with-at-least-32-bytes".into()),
            ttl: Some("120".into()),
        };
        let first = servers_from(&settings, account, 1_000).unwrap();
        assert_eq!(first[0].expires_in_secs, None, "STUN has no credentials");
        assert_eq!(first[1].expires_in_secs, Some(120));
        assert_eq!(first[1].username, format!("1120:{account}"));
        // A refresh (an ICE restart) mints new credentials expiring later.
        let refreshed = servers_from(&settings, account, 1_100).unwrap();
        assert_eq!(refreshed[1].username, format!("1220:{account}"));
        assert_ne!(refreshed[1].credential, first[1].credential);
        // Same input, same HMAC: coturn can verify it statelessly.
        assert_eq!(
            servers_from(&settings, account, 1_000).unwrap()[1].credential,
            first[1].credential
        );
        let invalid = Settings {
            ttl: Some("5".into()),
            ..settings
        };
        assert!(servers_from(&invalid, account, 1_000).is_err());
    }
}
