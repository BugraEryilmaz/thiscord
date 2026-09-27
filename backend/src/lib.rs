pub mod api;
pub mod auth;
pub mod db;
pub mod permissions;
pub mod schema;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
