pub mod api;
pub mod auth;
pub mod chat;
pub mod db;
pub mod permissions;
pub mod schema;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub mod tls;
pub mod voice;
