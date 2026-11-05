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
use thiscord_shared::{
    AccountId, ChannelId, GuildId,
    chat::{ChatMessage, OnlineMember},
};

/// Matches the per-socket event page; a full page falls back to a private read.
const PAGE: usize = 100;

pub(super) struct Feed {
    key: (GuildId, ChannelId),
    published: AtomicU64,
    read: Mutex<Option<Read>>,
    /// The same protocol for presence/typing publications and the shared list.
    presence_published: AtomicU64,
    presence: Mutex<Option<(u64, Vec<OnlineMember>)>>,
}
struct Read {
    /// `published` before this read's snapshot: every counted event is included.
    covered: u64,
    /// Events after this sequence, in order, at most one page.
    after: i64,
    events: Vec<(i64, ChatMessage)>,
}
impl Read {
    /// Whether this read contains every event a subscriber at `cursor` needs,
    /// given the publications counted when it was woken (`needed`).
    fn covers(&self, needed: u64, cursor: i64) -> bool {
        self.covered >= needed && self.after <= cursor
    }
    /// The subscriber's events, or `None` when it needs a private read: a full
    /// page may continue past this read, and reactions carry a per-account flag.
    fn after_cursor(&self, cursor: i64) -> Option<Vec<(i64, ChatMessage)>> {
        if self.events.len() >= PAGE {
            return None;
        }
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|(sequence, _)| *sequence > cursor)
            .cloned()
            .collect();
        events
            .iter()
            .all(|(_, message)| message.reactions.is_empty())
            .then_some(events)
    }
}
/// The last subscriber dropping a feed removes its registry entry too.
impl Drop for Feed {
    fn drop(&mut self) {
        let mut feeds = registry().lock().unwrap();
        if feeds
            .get(&self.key)
            .is_some_and(|feed| feed.strong_count() == 0)
        {
            feeds.remove(&self.key);
        }
    }
}

type Registry = Mutex<HashMap<(GuildId, ChannelId), Weak<Feed>>>;
fn registry() -> &'static Registry {
    static FEEDS: OnceLock<Registry> = OnceLock::new();
    FEEDS.get_or_init(Default::default)
}

/// Held by each channel subscription; the feed lives while anyone holds it.
/// No `Arc<Feed>` is ever dropped while the registry lock is held (`Drop` takes it).
pub(super) fn subscribe(guild: GuildId, channel: ChannelId) -> Arc<Feed> {
    let mut feeds = registry().lock().unwrap();
    if let Some(feed) = feeds.get(&(guild, channel)).and_then(Weak::upgrade) {
        return feed;
    }
    let feed = Arc::new(Feed {
        key: (guild, channel),
        published: AtomicU64::new(0),
        read: Mutex::new(None),
        presence_published: AtomicU64::new(0),
        presence: Mutex::new(None),
    });
    feeds.insert((guild, channel), Arc::downgrade(&feed));
    feed
}

/// After a message event commits, before subscribers are woken.
pub(super) fn published(guild: GuildId, channel: ChannelId) {
    if let Some(feed) = live(guild, channel) {
        feed.published.fetch_add(1, SeqCst);
    }
}
/// After a presence or typing change commits, before subscribers are woken.
pub(super) fn presence_published(guild: GuildId, channel: ChannelId) {
    if let Some(feed) = live(guild, channel) {
        feed.presence_published.fetch_add(1, SeqCst);
    }
}
fn live(guild: GuildId, channel: ChannelId) -> Option<Arc<Feed>> {
    registry()
        .lock()
        .unwrap()
        .get(&(guild, channel))
        .and_then(Weak::upgrade)
}

impl Feed {
    /// Like `events` without waiting or reading: `None` unless the last shared
    /// read already covers every publication and this subscriber's cursor. Never
    /// blocks, so async tasks may call it; a read in progress counts as a miss.
    pub(super) fn cached(&self, cursor: i64) -> Option<Vec<(i64, ChatMessage)>> {
        let needed = self.published.load(SeqCst);
        let read = self.read.try_lock().ok()?;
        read.as_ref()
            .filter(|r| r.covers(needed, cursor))?
            .after_cursor(cursor)
    }

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
        if !read.as_ref().is_some_and(|r| r.covers(needed, cursor)) {
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
        Ok(read.as_ref().unwrap().after_cursor(cursor))
    }
}

impl Feed {
    /// The channel's presence list for an authorized subscriber, read at most
    /// once per publication.
    pub(super) fn presence(
        &self,
        pool: &DbPool,
        guild: GuildId,
        channel: ChannelId,
    ) -> Result<Vec<OnlineMember>, Failure> {
        let needed = self.presence_published.load(SeqCst);
        let mut read = self.presence.lock().unwrap();
        if !read.as_ref().is_some_and(|(covered, _)| *covered >= needed) {
            let covered = self.presence_published.load(SeqCst);
            let mut c = connection(pool)?;
            let members = c
                .build_transaction()
                .read_only()
                .repeatable_read()
                .run(|c| super::store::presence_list(c, guild, channel))?;
            *read = Some((covered, members));
        }
        Ok(read.as_ref().unwrap().1.clone())
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

    fn message(sequence: i64, reacted: bool) -> (i64, ChatMessage) {
        let mut message: ChatMessage = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "client_id": uuid::Uuid::new_v4(),
            "guild_id": uuid::Uuid::nil(), "channel_id": uuid::Uuid::nil(),
            "author_id": null, "username": "u", "content": "c", "mentions": [],
            "created_at": "2026-01-01T00:00:00Z", "edited_at": null,
            "deleted": false, "revision": 1, "sequence": sequence}))
        .unwrap();
        if reacted {
            message.reactions = serde_json::from_value(
                serde_json::json!([{"emoji": "👍", "count": 1, "mine": false}]),
            )
            .unwrap();
        }
        (sequence, message)
    }

    #[test]
    fn reads_cover_later_cursors_and_newer_publications_need_a_new_read() {
        let read = Read {
            covered: 3,
            after: 10,
            events: vec![message(11, false), message(12, false)],
        };
        // Woken by publication 3 or earlier, at or past the read's start.
        assert!(read.covers(3, 10) && read.covers(2, 11));
        // A later publication, or a subscriber behind the read's start, rereads.
        assert!(!read.covers(4, 10) && !read.covers(3, 9));
        let sequences = |events: Option<Vec<(i64, ChatMessage)>>| {
            events.map(|e| e.iter().map(|(s, _)| *s).collect::<Vec<_>>())
        };
        assert_eq!(sequences(read.after_cursor(10)), Some(vec![11, 12]));
        assert_eq!(sequences(read.after_cursor(11)), Some(vec![12]));
        assert_eq!(sequences(read.after_cursor(12)), Some(vec![]));
    }

    #[test]
    fn full_pages_and_reactions_fall_back_to_private_reads() {
        let reacted = Read {
            covered: 1,
            after: 0,
            events: vec![message(1, false), message(2, true)],
        };
        assert!(reacted.after_cursor(0).is_none());
        // The reacted event is behind this cursor, so the shared read still serves it.
        assert_eq!(reacted.after_cursor(2).map(|e| e.len()), Some(0));
        let full = Read {
            covered: 1,
            after: 0,
            events: (1..=PAGE as i64).map(|n| message(n, false)).collect(),
        };
        assert!(full.after_cursor(0).is_none());
    }

    #[test]
    fn cached_reads_serve_only_covered_publications_without_waiting() {
        let (guild, channel) = ids();
        let feed = subscribe(guild, channel);
        assert!(feed.cached(0).is_none());
        *feed.read.lock().unwrap() = Some(Read {
            covered: 0,
            after: 0,
            events: vec![message(1, false)],
        });
        assert_eq!(feed.cached(0).map(|e| e.len()), Some(1));
        // A newer publication needs a new read; so does a read in progress.
        published(guild, channel);
        assert!(feed.cached(0).is_none());
        let held = feed.read.lock().unwrap();
        assert!(feed.cached(0).is_none());
        drop(held);
    }

    #[test]
    fn dropping_the_last_subscriber_removes_the_registry_entry() {
        let (guild, channel) = ids();
        let feed = subscribe(guild, channel);
        let second = subscribe(guild, channel);
        drop(feed);
        assert!(registry().lock().unwrap().contains_key(&(guild, channel)));
        drop(second);
        assert!(!registry().lock().unwrap().contains_key(&(guild, channel)));
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
