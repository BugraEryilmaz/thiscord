use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;

/// Bounded at deserialization too: never trust an unchecked client page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct PageSize(u16);

impl PageSize {
    pub const MAX: u16 = 100;

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self(50)
    }
}

impl TryFrom<u16> for PageSize {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if (1..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err("page size must be between 1 and 100")
        }
    }
}

impl From<PageSize> for u16 {
    fn from(value: PageSize) -> Self {
        value.0
    }
}

/// Versioned keyset cursor for ascending (created_at, UUID) ordering.
/// PostgreSQL timestamps have microsecond precision. UUID breaks timestamp ties.
/// This is an untrusted position, not an authorization token: list handlers must
/// still filter by membership/permissions before applying it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PageCursor {
    created_at: Timestamp,
    id: Uuid,
}

impl PageCursor {
    pub fn new(created_at: Timestamp, id: Uuid) -> Self {
        Self {
            created_at: Timestamp::from_timestamp_micros(created_at.timestamp_micros())
                .expect("a valid timestamp truncated to microseconds remains valid"),
            id,
        }
    }

    pub fn position(&self) -> (Timestamp, Uuid) {
        (self.created_at, self.id)
    }
}

impl From<PageCursor> for String {
    fn from(value: PageCursor) -> Self {
        let mut bytes = [0; 25];
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&value.created_at.timestamp_micros().to_be_bytes());
        bytes[9..].copy_from_slice(value.id.as_bytes());
        URL_SAFE_NO_PAD.encode(bytes)
    }
}

impl TryFrom<String> for PageCursor {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Reject oversized values before allocating decoded data.
        if value.len() != 34 {
            return Err("invalid cursor length");
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| "invalid cursor encoding")?;
        if bytes.len() != 25 || bytes[0] != 1 {
            return Err("unsupported cursor format");
        }
        let micros = i64::from_be_bytes(
            bytes[1..9]
                .try_into()
                .map_err(|_| "invalid cursor timestamp")?,
        );
        let created_at =
            Timestamp::from_timestamp_micros(micros).ok_or("invalid cursor timestamp")?;
        let id = Uuid::from_slice(&bytes[9..]).map_err(|_| "invalid cursor identifier")?;
        Ok(Self { created_at, id })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    #[serde(default)]
    pub limit: PageSize,
    pub after: Option<PageCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<PageCursor>,
}
