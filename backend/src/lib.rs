pub(crate) mod access;
pub mod api;
pub mod auth;
#[path = "../../bots/server/mod.rs"]
pub mod bots;
pub mod chat;
pub mod db;
pub mod friends;
pub mod limits;
pub mod notifications;
mod performance;
pub mod permissions;
mod retry;
pub mod safety;
pub mod schema;
pub mod shutdown;
pub mod soundboard;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub mod admin;
pub mod tls;
pub mod voice;
pub mod web_app;
pub mod welcome;

pub mod search;
