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
    Pool::builder()
        .max_size(5)
        .connection_timeout(CHECKOUT_TIMEOUT)
        .connection_customizer(Box::new(QueryTimeouts))
        .build(ConnectionManager::<PgConnection>::new(database_url))
}

/// Call from a blocking task at startup (or via --migrate-only).
pub fn connect_and_migrate(database_url: &str) -> Result<DbPool, BoxError> {
    let pool = connect(database_url)?;
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
