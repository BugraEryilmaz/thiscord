//! Shared per-channel message event reads for socket fan-out.
//!
//! A committed message wakes every subscriber of its channel. Without sharing,
//! each subscriber reads the same new events from the database. Here the first
//! subscriber to need them reads once and the others reuse that read. Reuse is
//! a read optimization only: each socket still authorizes the channel through
//! its own epoch-fenced grant and delivers under its own access permits.
//!
//! Correctness rests on ordering: `published` is incremented after a message
//! event commits and before any socket is woken. A read records the counter
//! value seen *before* its database snapshot, so it contains every event whose
//! publication was counted. A woken socket needs a read covering at least the
//! counter value it observes. Same-channel writes are serialized by the channel
//! row lock, so sequences commit in order within a channel.
use crate::auth::{Failure, store::connection};
use crate::db::DbPool;
use diesel::PgConnection;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering::SeqCst},
    },
};
use thiscord_shared::{AccountId, ChannelId, GuildId, chat::ChatMessage};

/// Matches the per-socket event page; a full page falls back to a private read.
const PAGE: usize = 100;

#[derive(Default)]
pub(super) struct Feed {
    published: AtomicU64,
    read: Mutex<Option<Read>>,
}
struct Read {
    /// `published` before this read's snapshot: every counted event is included.
    covered: u64,
    /// Events after this sequence, in order, at most one page.
    after: i64,
    events: Vec<(i64, ChatMessage)>,
}

type Registry = Mutex<HashMap<(GuildId, ChannelId), Weak<Feed>>>;
fn registry() -> &'static Registry {
    static FEEDS: OnceLock<Registry> = OnceLock::new();
    FEEDS.get_or_init(Default::default)
}

/// Held by each channel subscription; the feed lives while anyone holds it.
pub(super) fn subscribe(guild: GuildId, channel: ChannelId) -> Arc<Feed> {
    let mut feeds = registry().lock().unwrap();
    if let Some(feed) = feeds.get(&(guild, channel)).and_then(Weak::upgrade) {
        return feed;
    }
    feeds.retain(|_, feed| feed.strong_count() > 0);
    let feed = Arc::new(Feed::default());
    feeds.insert((guild, channel), Arc::downgrade(&feed));
    feed
}

/// After a message event commits, before subscribers are woken.
pub(super) fn published(guild: GuildId, channel: ChannelId) {
    let feed = registry()
        .lock()
        .unwrap()
        .get(&(guild, channel))
        .and_then(Weak::upgrade);
    if let Some(feed) = feed {
        feed.published.fetch_add(1, SeqCst);
    }
}

impl Feed {
    /// Events after `cursor` for an authorized subscriber of this channel, or
    /// `None` when they need a private read: more than one page, or reactions
    /// (whose `mine` flag is per account). Subscribers of a busy channel wait on
    /// one read instead of each checking out a database connection.
    pub(super) fn events(
        &self,
        pool: &DbPool,
        guild: GuildId,
        channel: ChannelId,
        cursor: i64,
    ) -> Result<Option<Vec<(i64, ChatMessage)>>, Failure> {
        let needed = self.published.load(SeqCst);
        let mut read = self.read.lock().unwrap();
        if !read
            .as_ref()
            .is_some_and(|r| r.covered >= needed && r.after <= cursor)
        {
            let covered = self.published.load(SeqCst);
            let mut c = connection(pool)?;
            let events = c
                .build_transaction()
                .read_only()
                .repeatable_read()
                .run(|c| shared_events(c, guild, channel, cursor))?;
            *read = Some(Read {
                covered,
                after: cursor,
                events,
            });
        }
        let read = read.as_ref().unwrap();
        if read.events.len() >= PAGE {
            return Ok(None);
        }
        let events: Vec<_> = read
            .events
            .iter()
            .filter(|(sequence, _)| *sequence > cursor)
            .cloned()
            .collect();
        Ok(events
            .iter()
            .all(|(_, message)| message.reactions.is_empty())
            .then_some(events))
    }
}

/// The same event page as a private poll, hydrated for no particular account.
fn shared_events(
    c: &mut PgConnection,
    guild: GuildId,
    channel: ChannelId,
    cursor: i64,
) -> Result<Vec<(i64, ChatMessage)>, Failure> {
    let mut events = super::store::events(c, guild, channel, cursor)?;
    let mut messages: Vec<_> = events.iter().map(|(_, m)| m.clone()).collect();
    super::rich::hydrate(c, AccountId::from_uuid(uuid::Uuid::nil()), &mut messages)?;
    for ((_, event), message) in events.iter_mut().zip(messages) {
        *event = message;
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (GuildId, ChannelId) {
        (
            GuildId::from_uuid(uuid::Uuid::new_v4()),
            ChannelId::from_uuid(uuid::Uuid::new_v4()),
        )
    }

    #[test]
    fn publications_count_only_for_live_feeds_and_feeds_are_shared() {
        let (guild, channel) = ids();
        published(guild, channel);
        let feed = subscribe(guild, channel);
        assert_eq!(feed.published.load(SeqCst), 0);
        let same = subscribe(guild, channel);
        assert!(Arc::ptr_eq(&feed, &same));
        published(guild, channel);
        assert_eq!(feed.published.load(SeqCst), 1);
        drop((feed, same));
        let fresh = subscribe(guild, channel);
        assert_eq!(fresh.published.load(SeqCst), 0);
    }
}
