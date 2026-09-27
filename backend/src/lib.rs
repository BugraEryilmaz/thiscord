pub mod api;
pub mod db;
pub mod schema;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
