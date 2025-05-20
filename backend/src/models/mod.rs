use diesel::{r2d2::{ConnectionManager, Pool, PooledConnection}, PgConnection};

mod user;

pub use user::*;

use crate::err::Error;

#[derive(Clone, Debug)]
pub struct PostgresBackend {
    pub pool: Pool<ConnectionManager<PgConnection>>
}

impl PostgresBackend {
    pub fn new(pool: Pool<ConnectionManager<PgConnection>>) -> Self {
        Self { pool }
    }
    pub fn get_connection(&self) -> Result<PooledConnection<ConnectionManager<PgConnection>>, Error> {
        self.pool.get().map_err(|e| e.into())
    }
}
pub type AuthSession = axum_login::AuthSession<PostgresBackend>;