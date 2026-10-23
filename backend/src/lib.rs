pub(crate) mod access;
pub mod api;
pub mod auth;
#[path = "../../bots/server/mod.rs"]
pub mod bots;
pub mod chat;
pub mod db;
pub mod friends;
pub mod notifications;
mod performance;
pub mod permissions;
pub mod safety;
pub mod schema;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub mod admin;
pub mod tls;
pub mod voice;
pub mod welcome;

pub mod search;
