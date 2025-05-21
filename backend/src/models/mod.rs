use diesel::{r2d2::{ConnectionManager, Pool, PooledConnection}, PgConnection};

mod user;

pub use user::*;

use crate::{utils::GmailBackend, Error};

#[derive(Clone, Debug)]
pub struct Backend {
    pub pool: Pool<ConnectionManager<PgConnection>>,
    pub email: GmailBackend,
}

impl Backend {
    pub fn new(pool: Pool<ConnectionManager<PgConnection>>) -> Self {
        Self { pool, email: GmailBackend::new() }
    }
    pub fn get_connection(&self) -> Result<PooledConnection<ConnectionManager<PgConnection>>, Error> {
        self.pool.get().map_err(|e| e.into())
    }
}
pub type AuthSession = axum_login::AuthSession<Backend>;