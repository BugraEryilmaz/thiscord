use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use thiscord_shared::{AccountId, ChannelId, GuildId};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(super) enum Change {
    Message(GuildId, ChannelId),
    Presence(GuildId, ChannelId),
    Read(GuildId, AccountId),
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Versions {
    pub messages: u64,
    pub presence: u64,
    pub unread: u64,
}

struct Route {
    guild: GuildId,
    channel: Option<ChannelId>,
    account: AccountId,
    sender: watch::Sender<Versions>,
}

#[derive(Default)]
pub(super) struct Notifications(Mutex<HashMap<Uuid, Route>>);

pub(super) fn notifications() -> &'static Notifications {
    static ROUTES: OnceLock<Notifications> = OnceLock::new();
    ROUTES.get_or_init(Notifications::default)
}

pub(super) struct Listener<'a> {
    routes: &'a Notifications,
    id: Uuid,
    pub receiver: watch::Receiver<Versions>,
}

impl Drop for Listener<'_> {
    fn drop(&mut self) {
        self.routes.0.lock().unwrap().remove(&self.id);
    }
}

impl Notifications {
    pub fn subscribe(
        &self,
        guild: GuildId,
        channel: Option<ChannelId>,
        account: AccountId,
    ) -> Listener<'_> {
        let (sender, receiver) = watch::channel(Versions::default());
        let id = Uuid::new_v4();
        self.0.lock().unwrap().insert(
            id,
            Route {
                guild,
                channel,
                account,
                sender,
            },
        );
        Listener {
            routes: self,
            id,
            receiver,
        }
    }

    // At most one watch value per live subscription, independent of event volume.
    // Routing metadata is only a wake hint; delivery still authorizes in the DB.
    pub fn publish(&self, change: Change) {
        for route in self.0.lock().unwrap().values() {
            route.sender.send_if_modified(|v| match change {
                Change::Message(guild, channel) if route.guild == guild => {
                    v.unread = v.unread.wrapping_add(1);
                    if route.channel == Some(channel) {
                        v.messages = v.messages.wrapping_add(1);
                    }
                    true
                }
                Change::Presence(guild, channel)
                    if route.guild == guild && route.channel == Some(channel) =>
                {
                    v.presence = v.presence.wrapping_add(1);
                    true
                }
                Change::Read(guild, account)
                    if route.guild == guild && route.account == account =>
                {
                    v.unread = v.unread.wrapping_add(1);
                    true
                }
                _ => false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_work_by_guild_channel_account_and_kind() {
        let routes = Notifications::default();
        let guild = GuildId::from_uuid(Uuid::new_v4());
        let channel = ChannelId::from_uuid(Uuid::new_v4());
        let account = AccountId::from_uuid(Uuid::new_v4());
        let mut selected = routes.subscribe(guild, Some(channel), account);
        let other_channel =
            routes.subscribe(guild, Some(ChannelId::from_uuid(Uuid::new_v4())), account);
        let guild_only = routes.subscribe(guild, None, AccountId::from_uuid(Uuid::new_v4()));
        let foreign = routes.subscribe(GuildId::from_uuid(Uuid::new_v4()), Some(channel), account);
        routes.publish(Change::Presence(guild, channel));
        assert_eq!(selected.receiver.borrow_and_update().presence, 1);
        assert!(!other_channel.receiver.has_changed().unwrap());
        assert!(!guild_only.receiver.has_changed().unwrap());
        routes.publish(Change::Read(guild, account));
        assert_eq!(selected.receiver.borrow_and_update().unread, 1);
        assert_eq!(other_channel.receiver.borrow().unread, 1);
        assert!(!guild_only.receiver.has_changed().unwrap());
        routes.publish(Change::Message(guild, channel));
        assert_eq!(
            *selected.receiver.borrow(),
            Versions {
                messages: 1,
                presence: 1,
                unread: 2
            }
        );
        assert_eq!(other_channel.receiver.borrow().messages, 0);
        assert_eq!(guild_only.receiver.borrow().unread, 1);
        assert!(!foreign.receiver.has_changed().unwrap());
        drop(selected);
        drop(other_channel);
        drop(guild_only);
        drop(foreign);
        assert!(routes.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bursts_coalesce_without_losing_other_kinds_or_later_changes() {
        let routes = Notifications::default();
        let guild = GuildId::from_uuid(Uuid::new_v4());
        let channel = ChannelId::from_uuid(Uuid::new_v4());
        let account = AccountId::from_uuid(Uuid::new_v4());
        let mut listener = routes.subscribe(guild, Some(channel), account);
        routes.publish(Change::Message(guild, channel));
        for _ in 0..1000 {
            routes.publish(Change::Read(guild, account));
        }
        routes.publish(Change::Presence(guild, channel));
        listener.receiver.changed().await.unwrap();
        let batch = *listener.receiver.borrow_and_update();
        assert_eq!(
            batch,
            Versions {
                messages: 1,
                presence: 1,
                unread: 1001
            }
        );
        assert!(!listener.receiver.has_changed().unwrap());
        // A commit during delivery remains pending for the next pass.
        routes.publish(Change::Read(guild, account));
        listener.receiver.changed().await.unwrap();
        assert_eq!(listener.receiver.borrow().unread, batch.unread + 1);
        drop(listener);
        let replacement = routes.subscribe(guild, None, account);
        assert_eq!(*replacement.receiver.borrow(), Versions::default());
        routes.publish(Change::Presence(guild, channel));
        assert!(!replacement.receiver.has_changed().unwrap());
    }
}
