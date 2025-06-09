use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::session)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Session {
    pub id: i32,
    pub token: String,
}

impl Session {
    pub fn new(token: String) -> Self {
        Session { id: 0, token }
    }
    
    pub fn get(mut conn: SqliteConnection) -> Result<Self, diesel::result::Error> {
        use crate::schema::session::dsl::*;
        Ok(session
            .filter(id.eq(0))
            .first::<Session>(&mut conn)
            .optional()?
            .unwrap_or_default())
    }

    pub fn save(&self, mut conn: SqliteConnection) -> Result<(), diesel::result::Error> {
        use crate::schema::session::dsl::*;
        diesel::insert_into(session)
            .values(self)
            .on_conflict(id)
            .do_update()
            .set(token.eq(&self.token))
            .execute(&mut conn)?;
        Ok(())
    }
}

impl Default for Session {
    fn default() -> Self {
        Session {
            id: 0,
            token: String::new(),
        }
    }
}