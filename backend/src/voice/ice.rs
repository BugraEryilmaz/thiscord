use base64::Engine;
use hmac::{Hmac, Mac};
use thiscord_shared::{AccountId, voice::IceServer};
pub(super) fn servers(account: AccountId) -> Result<Vec<IceServer>, String> {
    let mut servers = vec![];
    if let Ok(url) = std::env::var("THISCORD_STUN_URL") {
        if !url.starts_with("stun:") {
            return Err("Invalid STUN configuration".into());
        }
        servers.push(IceServer {
            urls: vec![url],
            username: String::new(),
            credential: String::new(),
        });
    }
    if let Ok(url) = std::env::var("THISCORD_TURN_URL") {
        if !url.starts_with("turn:") && !url.starts_with("turns:") {
            return Err("Invalid TURN configuration".into());
        }
        let secret = std::env::var("THISCORD_TURN_SECRET").map_err(|_| "TURN secret missing")?;
        if secret.len() < 32 {
            return Err("TURN secret must contain at least 32 bytes".into());
        }
        let expires = chrono::Utc::now().timestamp() + 3600;
        let username = format!("{expires}:{account}");
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes())
            .map_err(|_| "Invalid TURN secret")?;
        mac.update(username.as_bytes());
        let credential =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        servers.push(IceServer {
            urls: vec![url],
            username,
            credential,
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
