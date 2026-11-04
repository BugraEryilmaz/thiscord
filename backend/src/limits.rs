//! Per-process connection admission limits. Connections beyond a limit are
//! refused with the shared `rate_limited` error before any database work, so
//! admitted users keep their capacity. See docs/load-benchmarks.md for the
//! measurements behind the defaults.
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::BoxError;

pub const SOCKETS_VAR: &str = "THISCORD_MAX_SOCKETS";
pub const VOICE_VAR: &str = "THISCORD_MAX_VOICE_CONNECTIONS";
/// Each of the chat, friends and notification socket types admits this many.
/// A signed-in desktop or browser client holds one of each.
pub const DEFAULT_SOCKETS: usize = 512;
/// Concurrent voice/video connections (SFU peers), including test bots.
pub const DEFAULT_VOICE: usize = 128;
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

/// One socket type's admission slots, held for the socket's lifetime.
#[derive(Clone)]
pub(crate) struct SocketSlots(pub Arc<Semaphore>);
impl SocketSlots {
    pub(crate) fn new(limit: usize) -> Self {
        Self(Arc::new(Semaphore::new(limit)))
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
}
