//! Database access stays on the backend. Run blocking pool operations and Diesel
//! queries inside `tokio::task::spawn_blocking` when adding request handlers.

use std::time::Duration;

use diesel::{
    PgConnection,
    connection::SimpleConnection,
    prelude::*,
    r2d2::{ConnectionManager, CustomizeConnection, Error, Pool},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use thiscord_shared::InstanceId;

use crate::{BoxError, schema::instance};

pub type DbPool = Pool<ConnectionManager<PgConnection>>;
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
pub const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(2);
/// Shared by HTTP, sockets, notifications and diagnostics. Raise only after the
/// performance monitors show checkout waits with quick queries.
pub const DEFAULT_POOL_SIZE: u32 = 5;
/// Keeps several backends and maintenance sessions under PostgreSQL's default
/// `max_connections` (100).
pub const MAX_POOL_SIZE: u32 = 32;
pub const POOL_SIZE_VAR: &str = "DATABASE_POOL_SIZE";

/// Parses the optional `DATABASE_POOL_SIZE` value: an integer in 1..=32.
pub fn pool_size(value: Option<&str>) -> Result<u32, BoxError> {
    let Some(value) = value else {
        return Ok(DEFAULT_POOL_SIZE);
    };
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|size| (1..=MAX_POOL_SIZE).contains(size))
        .ok_or_else(|| {
            format!("{POOL_SIZE_VAR} must be an integer from 1 to {MAX_POOL_SIZE}").into()
        })
}

#[derive(Debug)]
struct QueryTimeouts;

impl CustomizeConnection<PgConnection, Error> for QueryTimeouts {
    fn on_acquire(&self, connection: &mut PgConnection) -> Result<(), Error> {
        connection
            .batch_execute("SET statement_timeout = '2s'; SET lock_timeout = '1s';")
            .map_err(Error::QueryError)
    }
}

pub fn connect(database_url: &str) -> Result<DbPool, diesel::r2d2::PoolError> {
    connect_sized(database_url, DEFAULT_POOL_SIZE)
}

/// `size` must come from [`pool_size`]. Checkout stays bounded by `CHECKOUT_TIMEOUT`.
pub fn connect_sized(database_url: &str, size: u32) -> Result<DbPool, diesel::r2d2::PoolError> {
    Pool::builder()
        .max_size(size.clamp(1, MAX_POOL_SIZE))
        .connection_timeout(CHECKOUT_TIMEOUT)
        .connection_customizer(Box::new(QueryTimeouts))
        .build(ConnectionManager::<PgConnection>::new(database_url))
}

/// Call from a blocking task at startup (or via --migrate-only).
pub fn connect_and_migrate(database_url: &str) -> Result<DbPool, BoxError> {
    connect_and_migrate_sized(database_url, DEFAULT_POOL_SIZE)
}

pub fn connect_and_migrate_sized(database_url: &str, size: u32) -> Result<DbPool, BoxError> {
    let pool = connect_sized(database_url, size)?;
    pool.get_timeout(CHECKOUT_TIMEOUT)?
        .run_pending_migrations(MIGRATIONS)?;
    Ok(pool)
}

/// A real schema query, not just a cached pool status or TCP probe.
pub fn check_readiness(pool: &DbPool) -> Result<InstanceId, BoxError> {
    let mut connection = pool.get_timeout(CHECKOUT_TIMEOUT)?;
    let id = instance::table
        .filter(instance::singleton.eq(true))
        .select(instance::id)
        .first::<uuid::Uuid>(&mut connection)?;
    Ok(InstanceId::from_uuid(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_size_is_bounded_and_defaults_to_five() {
        assert_eq!(pool_size(None).unwrap(), 5);
        assert_eq!(pool_size(Some(" 12 ")).unwrap(), 12);
        assert_eq!(pool_size(Some("1")).unwrap(), 1);
        assert_eq!(pool_size(Some("32")).unwrap(), MAX_POOL_SIZE);
        for invalid in ["0", "33", "-1", "", "five", "5.0", "4294967296"] {
            assert!(pool_size(Some(invalid)).is_err(), "{invalid}");
        }
    }
}
