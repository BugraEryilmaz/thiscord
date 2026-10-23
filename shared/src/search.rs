//! Authorized message search; cursors are positions, never access grants.
use crate::{
    AccountId, ChannelId, GuildId, MessageId, Timestamp,
    ids::ConversationId,
    pagination::{PageCursor, PageSize},
};
use serde::{Deserialize, Serialize};
pub const SEARCH_PATH: &str = "/api/v1/search";
pub const MAX_QUERY_CHARS: usize = 200;
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    Guild {
        guild_id: GuildId,
        channel_id: Option<ChannelId>,
    },
    Private {
        conversation_id: Option<ConversationId>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    pub scope: Scope,
    pub query: String,
    /// Exact username/handle, optionally prefixed with @.
    pub person: Option<String>,
    pub after: Option<Timestamp>,
    pub before: Option<Timestamp>,
}
impl Filters {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.query.trim().is_empty()
            || self.query.chars().count() > MAX_QUERY_CHARS
            || self.query.chars().any(char::is_control)
        {
            return Err("Search needs 1 to 200 characters");
        }
        if self.person.as_ref().is_some_and(|s| {
            s.len() > 32
                || s.trim_start_matches('@').is_empty()
                || !s
                    .trim_start_matches('@')
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) {
            return Err("Use an exact username for the person filter");
        }
        if self.after.zip(self.before).is_some_and(|(a, b)| a >= b) {
            return Err("Search start must be before its end");
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Search {
        filters: Filters,
        #[serde(default)]
        cursor: Option<PageCursor>,
        #[serde(default)]
        limit: PageSize,
    },
    Context {
        scope: Scope,
        message_id: MessageId,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Hit {
    pub id: MessageId,
    pub scope: Scope,
    pub author_id: Option<AccountId>,
    pub username: String,
    pub content: String,
    pub created_at: Timestamp,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Response {
    pub messages: Vec<Hit>,
    pub next: Option<PageCursor>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_wire_reject_bad_filters() {
        let mut f = Filters {
            scope: Scope::Private {
                conversation_id: None,
            },
            query: "word".into(),
            person: None,
            after: None,
            before: None,
        };
        assert!(f.validate().is_ok());
        f.query = "a".repeat(201);
        assert!(f.validate().is_err());
        assert!(serde_json::from_value::<Request>(serde_json::json!({"action":"search","filters":{"scope":{"kind":"private","conversation_id":null},"query":"ok","person":null,"after":null,"before":null},"limit":101})).is_err());
    }
}
