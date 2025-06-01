use diesel::{
    PgConnection,
    r2d2::{ConnectionManager, Pool, PooledConnection},
};

mod user;
mod permissions;

pub use user::*;
pub use permissions::*;

use crate::{Error, utils::GmailBackend};

#[derive(Clone, Debug)]
pub struct Backend {
    pub pool: Pool<ConnectionManager<PgConnection>>,
    pub email: GmailBackend,
}

impl Backend {
    pub fn new(pool: Pool<ConnectionManager<PgConnection>>) -> Self {
        Self {
            pool,
            email: GmailBackend::new(),
        }
    }
    pub fn get_connection(
        &self,
    ) -> Result<PooledConnection<ConnectionManager<PgConnection>>, Error> {
        self.pool.get().map_err(|e| e.into())
    }
}
pub type AuthSession = axum_login::AuthSession<Backend>;
