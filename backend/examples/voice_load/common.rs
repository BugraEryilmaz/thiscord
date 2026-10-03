//! Local benchmark conventions, not application wire contracts.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, &'static str>;
pub const ORIGIN: &str = "http://tauri.localhost";

pub fn secret() -> Result<String> {
    let value = std::env::var("THISCORD_LOAD_SECRET").map_err(
        |_| "Set THISCORD_LOAD_SECRET on both machines (64 random hexadecimal characters)",
    )?;
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("THISCORD_LOAD_SECRET must be 64 hexadecimal characters");
    }
    Ok(value)
}

pub fn id(kind: &str, index: usize) -> Uuid {
    let digest = Sha256::digest(format!("thiscord-voice-load-v1/{kind}/{index}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

pub fn token(secret: &str, index: usize) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC key length");
    mac.update(format!("thiscord-voice-load-v1/session/{index}").as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

pub struct Args(BTreeMap<String, String>);
impl Args {
    pub fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let mut values = BTreeMap::new();
        while let Some(key) = args.next() {
            let value = match key.as_str() {
                "--help" | "--allow-insecure" | "--relay" | "--tls" => "true".into(),
                _ if key.starts_with("--") => args.next().ok_or("Missing option value")?,
                _ => return Err("Expected --option value; see --help"),
            };
            if values.insert(key, value).is_some() {
                return Err("Duplicate option");
            }
        }
        Ok(Self(values))
    }
    pub fn flag(&mut self, name: &str) -> bool {
        self.0.remove(name).is_some()
    }
    pub fn text(&mut self, name: &str, default: &str) -> String {
        self.0.remove(name).unwrap_or_else(|| default.into())
    }
    pub fn number(&mut self, name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
        let value = self
            .text(name, &default.to_string())
            .parse::<usize>()
            .map_err(|_| "Invalid numeric option")?;
        if !(min..=max).contains(&value) {
            return Err("Numeric option outside documented bounds");
        }
        Ok(value)
    }
    pub fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("Unknown option; see --help")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_credentials_are_domain_separated() {
        assert_eq!(token("first", 0), token("first", 0));
        assert_ne!(token("first", 0), token("second", 0));
        assert_ne!(token("first", 0), token("first", 1));
        assert_ne!(id("account", 0), id("channel", 0));
    }
}
