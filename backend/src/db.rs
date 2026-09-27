//! Database access stays on the backend. Run blocking pool operations and Diesel
//! queries inside `tokio::task::spawn_blocking` when adding request handlers.

use diesel::{
    PgConnection,
    r2d2::{ConnectionManager, Pool},
};

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

pub fn connect(database_url: &str) -> Result<DbPool, diesel::r2d2::PoolError> {
    Pool::builder()
        .max_size(5)
        .build(ConnectionManager::<PgConnection>::new(database_url))
}
