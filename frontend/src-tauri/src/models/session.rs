use diesel::prelude::*;
use front_shared::Session;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Queryable, Selectable, Insertable,
)]
#[diesel(table_name = crate::schema::session)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct SessionWString {
    pub id: i32,
    pub token: String,
    pub user_id: String,
    pub username: String,
}
impl From<SessionWString> for Session {
    fn from(session: SessionWString) -> Self {
        Session {
            id: session.id,
            token: session.token,
            user_id: Uuid::parse_str(&session.user_id).unwrap_or_default(),
            username: session.username,
        }
    }
}

impl From<Session> for SessionWString {
    fn from(session: Session) -> Self {
        SessionWString {
            id: session.id,
            token: session.token,
            user_id: session.user_id.to_string(),
            username: session.username,
        }
    }
}

pub trait SessionStore {
    fn new(token: String, user_id: Uuid, username: String) -> Self;
    fn get(conn: SqliteConnection) -> Result<Self, diesel::result::Error>
    where
        Self: Sized;
    fn save(&self, conn: SqliteConnection) -> Result<(), diesel::result::Error>;
}

impl SessionStore for Session {
    fn new(token: String, user_id: Uuid, username: String) -> Self {
        Session {
            id: 0,
            token,
            user_id,
            username,
        }
    }

    fn get(mut conn: SqliteConnection) -> Result<Self, diesel::result::Error> {
        use crate::schema::session::dsl::*;
        Ok(session
            .filter(id.eq(0))
            .first::<SessionWString>(&mut conn)
            .optional()?
            .unwrap_or_default()
            .into())
    }

    fn save(&self, mut conn: SqliteConnection) -> Result<(), diesel::result::Error> {
        use crate::schema::session::dsl::*;
        let session_w_string: SessionWString = self.clone().into();
        diesel::insert_into(session)
            .values(&session_w_string)
            .on_conflict(id)
            .do_update()
            .set((token.eq(&self.token), user_id.eq(&session_w_string.user_id)))
            .execute(&mut conn)?;
        Ok(())
    }
}

impl Default for SessionWString {
    fn default() -> Self {
        SessionWString {
            id: 0,
            token: String::new(),
            user_id: String::new(),
            username: String::new(),
        }
    }
}

