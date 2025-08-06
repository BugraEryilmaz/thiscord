use std::sync::{atomic::{AtomicI32, Ordering}, Arc};

use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::per_user_boost;

#[derive(Default)]
pub struct PerUserBoost {
    pub user_id: Option<Uuid>,
    pub boost_level: Arc<AtomicI32>,
}

impl PerUserBoost {
    pub fn get(conn: &mut SqliteConnection, user_id: Uuid) -> Self {
        // use crate::schema::per_user_boost::dsl::*;
        let user_boost: Result<PerUserBoostWString, diesel::result::Error> = per_user_boost::dsl::per_user_boost
            .filter(per_user_boost::dsl::user_id.eq(user_id.to_string()))
            .select(PerUserBoostWString::as_select())
            .first::<PerUserBoostWString>(conn);
        match user_boost {
            Ok(boost) => PerUserBoost {
                user_id: Some(user_id),
                boost_level: Arc::new(AtomicI32::new(boost.boost_level)),
            },
            Err(_) => PerUserBoost {
                user_id: Some(user_id),
                boost_level: Arc::new(AtomicI32::new(100)),
            },
        }
    }
    pub fn save(&self, conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
        if let Some(user_id) = self.user_id {
            diesel::insert_into(per_user_boost::dsl::per_user_boost)
                .values(PerUserBoostWString {
                    user_id: user_id.to_string(),
                    boost_level: self.boost_level.load(Ordering::Relaxed),
                })
                .on_conflict(per_user_boost::dsl::user_id)
                .do_update()
                .set(per_user_boost::dsl::boost_level.eq(self.boost_level.load(Ordering::Relaxed)))
                .execute(conn)?;
        }
        Ok(())
    }
}

#[derive(Selectable, Queryable, Insertable, AsChangeset)]
#[diesel(table_name = crate::schema::per_user_boost)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct PerUserBoostWString {
    pub user_id: String,
    pub boost_level: i32,
}
