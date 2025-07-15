use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TurnCreds {
    pub username: String,
    pub credential: String,
    pub realm: String,
    pub expiration: String, // ISO 8601 format
}
